//! Selected-image production, exact payload closure admission, durable history
//! publication, and cold replay. Persisted V3 input claims remain opaque and
//! cannot recreate read-closure authority.

use super::{FileSemanticRangeStore, history_v2};
use crate::ir_generation_store::{HistoryTypedV3RootClaim, TypedV3HistoryLocator};
use crate::{DurableSemanticObjectAdmission, ProducedSemanticTypedPlaneV3};
use backend_semantic::ir::{
    JumboRopeLimits, SemanticInputClaimV2, SemanticInputWitness,
    SemanticTypedPlaneVerificationTierV2, VerifiedTypedPlaneContentV2,
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

impl TypedV3HistoryGcPin {
    fn into_guard(self) -> GcPinGuard {
        self._guard
    }
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

    /// Produces, verifies, and durably admits typed V3 history from the exact
    /// owner-selected local native image. The live input witness must match
    /// the selected generation's persisted input claim. This records that
    /// claim for cold binding; it does not persist or recreate a read-closure
    /// preimage, so read-frontier reuse remains unproven.
    #[allow(clippy::too_many_arguments)]
    pub fn admit_selected_typed_v3_history_commit<S: crate::SelectedGenerationSource>(
        &self,
        target: &crate::SemanticTargetKey,
        parents: &[crate::HistoryCommitId],
        provenance: [u8; 32],
        input_witness: SemanticInputWitness,
        policies: crate::SemanticTypedPlaneBoundaryPoliciesV3,
        tier: SemanticTypedPlaneVerificationTierV2,
        jumbo_limits: JumboRopeLimits,
        source: &mut S,
    ) -> Result<crate::HistoryAdmissionReceipt, String> {
        if !input_witness.coverage().is_authorized_complete() {
            return Err("typed V3 history requires a live complete owner input witness".to_owned());
        }
        let pin = self.pin_typed_v3_history_admission()?;
        let selected = self.generations.current(target)?.ok_or_else(|| {
            "no selected native generation is available for typed V3 history".to_owned()
        })?;
        let selected_id = selected.identity();
        let selected_stamp = selected.selected_stamp();
        let selected_image = selected.image();
        let selected_image_identity = selected.image_identity();
        let selected_manifest_root = selected.manifest().root();
        let selected_build = selected.manifest().build();
        let selected_input_claim = SemanticInputClaimV2::from_witness(&selected.manifest().input());
        if selected_input_claim != SemanticInputClaimV2::from_witness(&input_witness) {
            return Err(
                "typed V3 input witness differs from the selected native generation claim"
                    .to_owned(),
            );
        }
        require_live_v3_selection(source, selected_stamp, selected_image)?;
        let mapped = self
            .find_semantic_image(target, selected_image)
            .map_err(|error| format!("open selected native image for typed V3 history: {error}"))?
            .ok_or_else(|| {
                "selected native image is missing from the local image store".to_owned()
            })?;
        if mapped.identity() != selected_image_identity
            || mapped.generation() != selected.semantic_generation()
        {
            return Err(
                "selected native image identity differs from its generation record".to_owned(),
            );
        }
        let produced = crate::ir_producer_store::produce_semantic_typed_plane_v3(
            &self.store,
            &mapped.view(),
            selected_build,
            input_witness,
            policies,
            tier,
            jumbo_limits,
        )?;
        let admission = self.verify_produced_typed_v3(&pin, &produced, tier, jumbo_limits)?;
        if produced.manifest().build() != selected_build
            || produced.manifest().input_claim() != selected_input_claim
        {
            return Err(
                "typed V3 producer manifest differs from the selected image input".to_owned(),
            );
        }
        require_live_v3_selection(source, selected_stamp, selected_image)?;

        let _state_lock = self.acquire_state_lock()?;
        let current = self.generations.current(target)?.ok_or_else(|| {
            "selected native generation disappeared during typed V3 production".to_owned()
        })?;
        if current.identity() != selected_id
            || current.selected_stamp() != selected_stamp
            || current.image() != selected_image
            || current.image_identity() != selected_image_identity
            || current.manifest().root() != selected_manifest_root
        {
            return Err("selected native generation changed during typed V3 production".to_owned());
        }
        let (_, pending_locators_remain) = self
            .generations
            .reconcile_pending_typed_v3_locators(target)?;
        if pending_locators_remain {
            return Err("typed V3 locator recovery remains bounded and must be retried".to_owned());
        }
        let locator_id = self
            .generations
            .typed_v3_locator_identity(admission.locator())?;
        let closure_id = admission.closure();
        let closure = backend_store::ArtifactClosureClaim::from_id(closure_id);
        let proposal = self
            .generations
            .propose_typed_v3_history_commit(
                target,
                parents,
                provenance,
                admission.content(),
                admission.locator(),
                closure,
                locator_id,
            )
            .map_err(|error| error.to_string())?;
        let commit = proposal.identity();
        self.generations
            .stage_typed_v3_locator(target, commit, admission.locator())?;
        let receipt = match self.generations.admit_typed_v3_history_proposal(
            proposal,
            crate::ir_generation_store::AdmittedHistoryPayloadRoot {
                closure: closure_id,
            },
            source,
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                self.generations
                    .reconcile_typed_v3_locator_admission(target, commit)
                    .map_err(|recovery| {
                        format!("{error}; typed V3 locator recovery failed: {recovery}")
                    })?;
                return Err(error);
            }
        };
        self.generations
            .finish_typed_v3_locator_admission(target, commit)?;

        let content = *admission.content();
        let closure_claim = closure;
        drop(admission);
        drop(produced);
        drop(mapped);
        let guard = pin.into_guard();
        receipt
            .with_gc_pin(std::sync::Arc::new(guard))
            .with_typed_v3_proof(
                content,
                input_witness,
                closure_claim,
                locator_id,
                self.store.root().to_path_buf(),
            )
    }

    /// Publishes a live V3 admission receipt with a named-ref compare-and-swap.
    /// The exact local selected generation and owner authority are rechecked
    /// under the shared state lock immediately before the CAS.
    pub fn publish_typed_v3_history_ref<S: crate::SelectedGenerationSource>(
        &self,
        target: &crate::SemanticTargetKey,
        kind: crate::HistoryRefKind,
        name: crate::HistoryRefName,
        expected: Option<crate::HistoryCommitId>,
        admission: &crate::HistoryAdmissionReceipt,
        source: &mut S,
    ) -> Result<crate::HistoryRefUpdateReceipt, String> {
        let proof = admission
            .typed_v3_publication_admission(self.store.root())
            .ok_or_else(|| {
                "typed V3 publication requires a live same-store verifier receipt".to_owned()
            })?;
        let commit = admission.commit();
        let _state_lock = self.acquire_state_lock()?;
        let current = self.generations.current(target)?.ok_or_else(|| {
            "selected native generation is missing at V3 ref publication".to_owned()
        })?;
        if current.identity() != commit.generation()
            || current.selected_stamp() != commit.selected_stamp()
            || current.manifest().root() != commit.manifest_root()
        {
            return Err("typed V3 commit no longer names the current local generation".to_owned());
        }
        require_live_v3_selection(source, current.selected_stamp(), current.image())?;
        let receipt = self.generations.compare_and_swap_typed_v3_history_ref(
            target,
            kind,
            name,
            expected,
            proof.identity(),
            &proof,
        )?;
        Ok(receipt)
    }

    /// Cold-revalidates the full typed closure before publishing a V3 history
    /// ref after process restart. The operation retains a fresh same-store GC
    /// pin through the ref CAS and does not recreate input read-frontier proof.
    pub fn publish_typed_v3_history_ref_cold(
        &self,
        target: &crate::SemanticTargetKey,
        kind: crate::HistoryRefKind,
        name: crate::HistoryRefName,
        expected: Option<crate::HistoryCommitId>,
        commit_id: crate::HistoryCommitId,
        tier: SemanticTypedPlaneVerificationTierV2,
        jumbo_limits: JumboRopeLimits,
    ) -> Result<crate::HistoryRefUpdateReceipt, String> {
        let pin = self.pin_typed_v3_history_admission()?;
        let snapshot = {
            let _state_lock = self.acquire_state_lock()?;
            self.generations
                .typed_v3_publication_snapshot(target, commit_id)?
        };
        let manifest = snapshot.locator().validate()?;
        let (content, closure) = verify_cold_v3_snapshot(
            &self.store,
            snapshot.claim(),
            snapshot.locator(),
            &manifest,
            tier,
            jumbo_limits,
        )?;
        let proof =
            crate::ir_generation_store::TypedV3HistoryPublicationAdmission::from_cold_verification(
                snapshot.identity(),
                content,
                manifest.input_claim(),
                closure,
                snapshot.claim().locator(),
                &pin._guard,
            );
        let _state_lock = self.acquire_state_lock()?;
        self.generations
            .revalidate_typed_v3_publication_snapshot(target, &snapshot)?;
        self.generations.compare_and_swap_typed_v3_history_ref(
            target,
            kind,
            name,
            expected,
            snapshot.identity(),
            &proof,
        )
    }

    /// Cold-replays a V3 commit reachable from the exact supplied named-ref
    /// tip. It validates the canonical manifest, V3 root claims, immutable
    /// locator, every closure member, all seven typed families, and jumbo
    /// ropes. The token explicitly exposes input authority as unproven.
    pub fn replay_typed_v3_history(
        &self,
        target: &crate::SemanticTargetKey,
        kind: crate::HistoryRefKind,
        name: &crate::HistoryRefName,
        commit_id: crate::HistoryCommitId,
        ancestry: &crate::HistoryRefAncestryProof,
        tier: SemanticTypedPlaneVerificationTierV2,
        jumbo_limits: JumboRopeLimits,
    ) -> Result<crate::TypedV3HistoryReplay, String> {
        let pin = self.pin_typed_v3_history_admission()?;
        let (commit, snapshot) = {
            let _state_lock = self.acquire_state_lock()?;
            self.validate_history_ref_proof(target, kind, name, commit_id, ancestry)?;
            let commit = self.generations.history_commit(target, commit_id)?;
            let snapshot = self
                .generations
                .typed_v3_publication_snapshot(target, commit_id)?;
            (commit, snapshot)
        };
        let claim = snapshot.claim();
        let locator = snapshot.locator().clone();
        let manifest = locator.validate()?;
        let (content, _) =
            verify_cold_v3_snapshot(&self.store, claim, &locator, &manifest, tier, jumbo_limits)?;
        {
            let _state_lock = self.acquire_state_lock()?;
            self.validate_history_ref_proof(target, kind, name, commit_id, ancestry)?;
        }
        Ok(crate::TypedV3HistoryReplay::new(
            commit,
            manifest,
            content,
            std::sync::Arc::new(pin.into_guard()),
        ))
    }
}

fn require_live_v3_selection<S: crate::SelectedGenerationSource>(
    source: &mut S,
    stamp: crate::SelectedGenerationStamp,
    image: backend_semantic::ir::SemanticPlaneImageKey,
) -> Result<(), String> {
    let observed = source
        .current_selected_generation()
        .map_err(|error| format!("read current selection for typed V3 history: {error}"))?;
    if observed != stamp {
        return Err("typed V3 history selected-generation stamp is stale".to_owned());
    }
    if !source
        .selected_image_is_current(stamp, image)
        .map_err(|error| format!("verify current image for typed V3 history: {error}"))?
    {
        return Err("typed V3 history image is no longer selected".to_owned());
    }
    Ok(())
}

fn verify_cold_v3_snapshot(
    store: &backend_store::FileStore,
    claim: HistoryTypedV3RootClaim,
    locator: &TypedV3HistoryLocator,
    manifest: &backend_semantic::ir::SemanticTypedPlaneManifestV2,
    tier: SemanticTypedPlaneVerificationTierV2,
    jumbo_limits: JumboRopeLimits,
) -> Result<
    (
        VerifiedTypedPlaneContentV2,
        backend_store::ArtifactClosureClaim,
    ),
    String,
> {
    if manifest.content_root_claim().as_bytes() != claim.content_root_claim().as_bytes()
        || manifest.generation_root_claim().as_bytes() != claim.generation_root_claim().as_bytes()
    {
        return Err("typed V3 commit roots differ from its cold manifest".to_owned());
    }
    let closure = claim.closure();
    let (content, durable_closure, _, _) = history_v2::verify_typed_v2_history_payload_closure(
        store,
        closure,
        locator.bridge(),
        manifest,
        tier,
        jumbo_limits,
    )?;
    if durable_closure.as_bytes() != closure.as_bytes()
        || !claim.content_root_claim().matches(content.content_root())
        || !claim
            .generation_root_claim()
            .matches(content.generation_root())
    {
        return Err("typed V3 history root failed cold closure verification".to_owned());
    }
    Ok((content, closure))
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
    use crate::ir_hydration::DurableSemanticSegmentStore;
    use crate::ir_hydration_store::history_v2::verify_typed_v2_history_payload_closure;
    use crate::{
        DurableSemanticRangeStore, SelectedGenerationSource, SelectedSemanticPlane,
        SemanticTypedPlaneBoundaryPoliciesV3,
    };
    use backend_semantic::ir::{
        BorrowedTree, CoreDeclarationRows, CorePayloadHash, DeclarationFamilyId,
        EntityAuthorityFacts, EntityVersion, FactAvailability, GenerationId, IrBuilder, ItemKind,
        JumboRopeLimits, LanguageProfile, RustEdition, SemanticBuildIdentity,
        SemanticImageIdentity, SemanticInputWitness, SemanticIrPlane, SemanticPlane,
        SemanticPlaneCatalog, SemanticPlaneCatalogEntry, SemanticPlaneCoverageScope,
        SemanticPlaneImageKey, SemanticPlaneManifest, SemanticPlaneSegment,
        SemanticPlaneSegmentBoundaryPolicy, TreeItemInput, VariantFingerprint, Visibility,
        encode_canonical_plane_family, encode_full_semantic_image, full_semantic_image_len,
    };
    use backend_semantic::vocabulary::Stage;
    use backend_store::{FileStore, TypedObject};
    use backend_version::{
        AdmittedProducerObservation, AuthorityScopeClaim, Coverage, CoverageAdmissionError,
        CoverageWitness, ObjectKey, ObjectVersion, ProducerObservationClaims,
        ProducerObservationVerifier, Schema, UntrustedProducerObservation, admit_complete_scope,
        admit_producer_observation,
    };
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

    struct TestInputScope;

    impl Schema for TestInputScope {
        const DOMAIN: u8 = 0x53;
        const TYPE: u16 = 0xfffc;
        type Value = [u8; 32];

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    struct TestCoverageVerifier;

    impl ProducerObservationVerifier for TestCoverageVerifier {
        type Error = CoverageAdmissionError;

        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            Ok(ProducerObservationClaims::new(
                observation.producer_identity(),
                observation.scope_root(),
                observation.context(),
                *blake3::hash(observation.evidence()).as_bytes(),
            ))
        }
    }

    fn complete_coverage(claim: AuthorityScopeClaim, nonce: u8) -> CoverageWitness {
        let observation = UntrustedProducerObservation::new(
            [0x61; 32],
            claim.scope_root(),
            [0x62; 32],
            vec![nonce, nonce.wrapping_add(1)],
        );
        let producer: AdmittedProducerObservation =
            admit_producer_observation(observation, &TestCoverageVerifier)
                .expect("test producer observation is admitted");
        CoverageWitness::Complete(
            admit_complete_scope(claim, producer).expect("test coverage scope matches"),
        )
    }

    fn live_input_witness() -> SemanticInputWitness {
        live_input_witness_with_root([0x71; 32])
    }

    fn live_input_witness_with_root(input_root: [u8; 32]) -> SemanticInputWitness {
        let version = ObjectVersion::<TestInputScope>::from_value(&[0x72; 32]);
        let claim = AuthorityScopeClaim::from_object_version(version);
        SemanticInputWitness::admitted(
            input_root,
            claim.scope_root(),
            complete_coverage(claim, 0x73),
        )
        .expect("selected test input witness is complete")
    }

    struct TestSelectedSource {
        stamp: crate::SelectedGenerationStamp,
        image: SemanticPlaneImageKey,
    }

    impl SelectedGenerationSource for TestSelectedSource {
        type Error = &'static str;

        fn current_selected_generation(
            &mut self,
        ) -> Result<crate::SelectedGenerationStamp, Self::Error> {
            Ok(self.stamp)
        }

        fn selected_image_is_current(
            &mut self,
            expected_stamp: crate::SelectedGenerationStamp,
            image: SemanticPlaneImageKey,
        ) -> Result<bool, Self::Error> {
            Ok(expected_stamp == self.stamp && image == self.image)
        }
    }

    fn selected_native_fixture(
        directory: &TestDirectory,
    ) -> (
        FileSemanticRangeStore,
        TestSelectedSource,
        crate::SemanticTargetKey,
        SemanticInputWitness,
    ) {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let target = crate::SemanticTargetKey::new(
            "pkg:cargo/typed-v3-history-test@1.0.0",
            "pkg:cargo/typed-v3-history-test@1.0.0",
            profile,
        )
        .expect("test target");
        let build = SemanticBuildIdentity::new(
            [0x11; 32],
            [0x12; 32],
            profile,
            Stage::LowerIr,
            [0x13; 32],
            [0x14; 32],
            [0x15; 32],
            [0x16; 32],
        );
        let input = live_input_witness();
        let versions = [EntityVersion {
            family: DeclarationFamilyId::from_raw([0x21; 16]),
            variant: VariantFingerprint::from_raw([0x22; 16]),
            core_payload: CorePayloadHash::from_raw([0x23; 16]),
        }];
        let items = [TreeItemInput {
            name: b"one",
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                members: FactAvailability::Captured,
                documentation: FactAvailability::Captured,
                attributes: FactAvailability::Captured,
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        }];
        let mut builder = IrBuilder::new();
        builder
            .set_language_profile(profile)
            .expect("set native test image profile");
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &[],
            })
            .expect("one-row native test image is valid");
        let ir = builder.finish().expect("finish native test image");
        let image_length = full_semantic_image_len(&ir).expect("plan native image");
        let mut image_bytes = vec![0; image_length];
        encode_full_semantic_image(&ir, &mut image_bytes).expect("encode native image");
        let semantic_generation = GenerationId::from_canonical_bytes(&image_bytes);
        let kind = backend_semantic::ir::SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let segments = encode_canonical_plane_family(
            &ir,
            &CoreDeclarationRows,
            input,
            backend_semantic::ir::MAX_SEMANTIC_SEGMENT_BYTES,
        )
        .expect("encode the selected native core plane");
        let descriptors = segments
            .iter()
            .map(|segment| segment.metadata().expect("build native segment claim"))
            .collect::<Vec<SemanticPlaneSegment>>();
        let claimed_plane = SemanticPlane::claimed(kind, descriptors.clone(), Coverage::Complete)
            .expect("build native selected plane claim");
        let plane_scope_version =
            ObjectVersion::<SemanticPlaneCoverageScope>::from_value(&claimed_plane.root());
        let plane_scope = AuthorityScopeClaim::from_object_version(plane_scope_version);
        let plane =
            SemanticPlane::admitted(kind, descriptors, complete_coverage(plane_scope, 0x74))
                .expect("admit selected native plane coverage");
        let manifest = SemanticPlaneManifest::new(semantic_generation, build, input, vec![plane])
            .expect("build complete selected native manifest");
        let image = SemanticPlaneImageKey::from_manifest(0, &manifest);
        let image_identity = SemanticImageIdentity::from_encoded_bytes(&image_bytes);
        let manifest_bytes = manifest.encode().expect("encode selected manifest");
        let manifest_length = u32::try_from(manifest_bytes.len()).expect("test manifest fits");
        let catalog = SemanticPlaneCatalog::new(vec![
            SemanticPlaneCatalogEntry::new(image, manifest_length).expect("catalog entry"),
        ])
        .expect("selected native catalog");
        let stamp = crate::SelectedGenerationStamp::checked(
            [0x31; 16],
            profile,
            [0x32; 32],
            1,
            [0x33; 32],
            [0x34; 32],
            catalog.root(),
        )
        .expect("selected owner stamp");
        let source = TestSelectedSource { stamp, image };
        let store = FileStore::open(directory.0.join("selected-cas"), 64 * 1024 * 1024)
            .expect("open selected V3 FileStore");
        let mut range_store = FileSemanticRangeStore::open(
            store,
            crate::TransportLimits {
                max_chunk: 16 * 1024,
                max_frame: 16 * 1024 + 192,
                ..crate::TransportLimits::default()
            },
        )
        .expect("open selected V3 range store");
        let total = u64::try_from(image_bytes.len()).expect("image length fits");
        let resume = range_store
            .stage_semantic_image_page(
                &target,
                image,
                image_identity,
                total,
                crate::ByteRange::new(0, total).expect("image range"),
                &image_bytes,
            )
            .expect("stage exact selected image");
        range_store
            .finish_semantic_image_transfer(&target, resume)
            .expect("admit exact selected image");
        let mut select_source = source_copy(&source);
        let selection = SelectedSemanticPlane::select(&mut select_source, &manifest, image, kind)
            .expect("select current native core plane");
        for (segment, descriptor) in segments
            .iter()
            .zip(manifest.plane(kind).unwrap().segments())
        {
            let payload = segment.bytes();
            let request = backend_semantic::ir::SemanticRangeRequest {
                manifest_root: manifest.root(),
                plane: kind,
                segment_id: descriptor.id_claim(),
                first_key: *descriptor.first_key(),
                last_key: *descriptor.last_key(),
                byte_length: descriptor.byte_length(),
            };
            let payload_length = u64::try_from(payload.len()).expect("segment length fits");
            range_store
                .stage_durable_range(
                    selection,
                    request,
                    crate::ByteRange::new(0, payload_length).expect("segment byte range"),
                    payload,
                )
                .expect("stage native core segment");
            let admitted = descriptor
                .admit(kind, payload)
                .expect("native segment matches its canonical claim");
            let mut verify = |bytes: &[u8]| {
                descriptor
                    .admit(kind, bytes)
                    .map(|_| ())
                    .map_err(|_| crate::ReplicationError::IdentityMismatch)
            };
            range_store
                .commit_and_read(selection, admitted, payload, &mut verify)
                .expect("persist selected native core segment");
        }
        range_store
            .commit_local_generation(
                &target,
                &catalog,
                image,
                &manifest,
                selection,
                &mut source_copy(&source),
            )
            .expect("commit selected native generation");
        (range_store, source, target, input)
    }

    fn source_copy(source: &TestSelectedSource) -> TestSelectedSource {
        TestSelectedSource {
            stamp: source.stamp,
            image: source.image,
        }
    }

    fn v3_test_policies() -> SemanticTypedPlaneBoundaryPoliciesV3 {
        let policy = SemanticPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(512, 1024, 4096)
            .expect("V3 test boundary policy");
        SemanticTypedPlaneBoundaryPoliciesV3::new(
            policy, policy, policy, policy, policy, policy, policy,
        )
    }

    #[test]
    fn selected_native_v3_publication_cas_and_cold_replay() {
        let directory = TestDirectory::create();
        let (store, mut source, target, input) = selected_native_fixture(&directory);
        let local_cache = crate::HistoryRefName::new("local-cache").expect("local ref name");
        let prior = store
            .history_ref(&target, crate::HistoryRefKind::Branch, &local_cache)
            .expect("read selected local-cache history")
            .expect("selected generation has a durable local-cache commit");
        let provenance = [0x42; 32];
        let mismatched_input = live_input_witness_with_root([0x7a; 32]);
        assert!(
            store
                .admit_selected_typed_v3_history_commit(
                    &target,
                    &[prior.commit()],
                    provenance,
                    mismatched_input,
                    v3_test_policies(),
                    SemanticTypedPlaneVerificationTierV2::Standard,
                    JumboRopeLimits::default(),
                    &mut source,
                )
                .is_err()
        );
        let admission = store
            .admit_selected_typed_v3_history_commit(
                &target,
                &[prior.commit()],
                provenance,
                input,
                v3_test_policies(),
                SemanticTypedPlaneVerificationTierV2::Standard,
                JumboRopeLimits::default(),
                &mut source,
            )
            .expect("produce and admit selected native image as V3 history");
        let commit_id = admission.commit().identity();
        assert!(matches!(
            admission.commit().generation_root(),
            crate::HistoryGenerationRoot::TypedV3(_)
        ));
        let typed_branch = crate::HistoryRefName::new("typed-v3-live").expect("V3 branch name");
        store
            .publish_typed_v3_history_ref(
                &target,
                crate::HistoryRefKind::Branch,
                typed_branch.clone(),
                None,
                &admission,
                &mut source,
            )
            .expect("publish exact live V3 receipt with branch CAS");
        drop(admission);
        drop(store);

        let cold_file_store = FileStore::open(directory.0.join("selected-cas"), 64 * 1024 * 1024)
            .expect("cold reopen selected V3 FileStore");
        let cold_store = FileSemanticRangeStore::open(
            cold_file_store,
            crate::TransportLimits {
                max_chunk: 16 * 1024,
                max_frame: 16 * 1024 + 192,
                ..crate::TransportLimits::default()
            },
        )
        .expect("cold reopen selected V3 range store");
        let cold_tag = crate::HistoryRefName::new("typed-v3-cold").expect("cold tag name");
        assert!(
            cold_store
                .publish_typed_v3_history_ref_cold(
                    &target,
                    crate::HistoryRefKind::Tag,
                    cold_tag.clone(),
                    Some(prior.commit()),
                    commit_id,
                    SemanticTypedPlaneVerificationTierV2::Standard,
                    JumboRopeLimits::default(),
                )
                .is_err()
        );
        cold_store
            .publish_typed_v3_history_ref_cold(
                &target,
                crate::HistoryRefKind::Tag,
                cold_tag,
                None,
                commit_id,
                SemanticTypedPlaneVerificationTierV2::Standard,
                JumboRopeLimits::default(),
            )
            .expect("cold closure verification permits exact tag CAS");
        let proof = cold_store
            .history_ref_ancestry_proof(
                &target,
                crate::HistoryRefKind::Tag,
                &crate::HistoryRefName::new("typed-v3-cold").expect("cold tag name"),
                commit_id,
            )
            .expect("cold named ref proves commit ancestry");
        let replay = cold_store
            .replay_typed_v3_history(
                &target,
                crate::HistoryRefKind::Tag,
                &crate::HistoryRefName::new("typed-v3-cold").expect("cold tag name"),
                commit_id,
                &proof,
                SemanticTypedPlaneVerificationTierV2::Standard,
                JumboRopeLimits::default(),
            )
            .expect("cold replay verifies exact persisted typed closure");
        assert_eq!(replay.commit().identity(), commit_id);
        assert_eq!(replay.manifest().input_claim(), replay.input_claim());
        assert_eq!(
            replay.input_replay_status(),
            crate::TypedV3HistoryInputReplayStatus::Unproven,
            "cold replay must not recreate a read-frontier proof from a stored claim"
        );
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
