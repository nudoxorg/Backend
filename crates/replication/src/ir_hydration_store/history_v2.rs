//! FileStore-backed cold replay for typed V2 history commits.

use super::{FileSemanticRangeStore, IO_BUFFER_BYTES, display_error, display_io};

use crate::{HistoryTypedV2JumboObject, HistoryTypedV2SegmentObject};
use backend_semantic::ir::{
    JUMBO_ROPE_MAX_LEAF_BYTES, JumboRopeLimits, JumboRopeObjectId, JumboRopeObjectKind,
    JumboRopeObjectSource, MAX_SEMANTIC_SEGMENT_BYTES, ROPE_NODE_WIRE_BYTES,
    SemanticTypedPlaneManifestV2, SemanticTypedPlaneVerificationTierV2,
    VerifiedTypedPlaneContentV2, verify_typed_plane_content_v2_with_jumbo_source,
};
use backend_store::{
    ArtifactBudget, ArtifactClosureClaim, ArtifactObjectReader, DurableManifest, FileStore,
    UntrustedObjectId,
};
use backend_version::SchemaIdentity;

const MAX_TYPED_V2_CLOSURE_OBJECTS: usize = 200_000;
const MAX_TYPED_V2_STANDARD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TYPED_V2_LARGE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_JUMBO_INTERIOR_BYTES: u64 = ROPE_NODE_WIRE_BYTES as u64;

#[cfg(test)]
thread_local! {
    static TYPED_V2_CLOSURE_REOPEN_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_typed_v2_closure_reopen_count() {
    TYPED_V2_CLOSURE_REOPEN_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn typed_v2_closure_reopen_count() -> usize {
    TYPED_V2_CLOSURE_REOPEN_COUNT.with(std::cell::Cell::get)
}

impl FileSemanticRangeStore {
    /// Admits a durable typed V2 history commit after reopening and verifying
    /// the exact FileStore closure and all semantic rows. This only persists a
    /// history commit; callers must publish it with the live proof-bearing
    /// receipt path or the cold re-admission path. The semantic input claim
    /// remains non-authorizing, so this method does not select V2 for
    /// production compilation or query routing.
    #[allow(clippy::too_many_arguments)]
    pub fn admit_typed_v2_history_commit<S: crate::SelectedGenerationSource>(
        &self,
        target: &crate::SemanticTargetKey,
        parents: &[crate::HistoryCommitId],
        provenance: [u8; 32],
        manifest: &SemanticTypedPlaneManifestV2,
        closure_claim: ArtifactClosureClaim,
        segment_objects: &[HistoryTypedV2SegmentObject],
        jumbo_objects: &[HistoryTypedV2JumboObject],
        tier: SemanticTypedPlaneVerificationTierV2,
        jumbo_limits: JumboRopeLimits,
        source: &mut S,
    ) -> Result<crate::HistoryAdmissionReceipt, String> {
        let gc_pin = self
            .store
            .pin_garbage_collection()
            .map_err(|error| format!("pin typed V2 history admission: {error:?}"))?;
        let expected_segments = manifest
            .resource_usage()
            .map_err(|error| format!("measure typed V2 history manifest: {error}"))?
            .segment_descriptors();
        crate::ir_generation_store::TypedV2HistoryLocator::preflight_admission_counts(
            expected_segments,
            segment_objects.len(),
            jumbo_objects.len(),
        )?;
        let manifest_bytes = manifest
            .canonical_bytes()
            .map_err(|error| format!("encode typed V2 history manifest: {error}"))?;
        let locator = crate::ir_generation_store::TypedV2HistoryLocator::from_admission_parts(
            manifest_bytes,
            expected_segments,
            segment_objects,
            jumbo_objects,
        )?;
        let decoded_manifest = locator.validate()?;
        if decoded_manifest != *manifest {
            return Err("typed V2 history manifest changed during canonical encoding".to_owned());
        }
        let closure = self
            .store
            .open_closure_claim(closure_claim)
            .map_err(|error| format!("open typed V2 history closure: {error:?}"))?;
        reopen_typed_v2_history_closure(
            &self.store,
            closure_claim,
            &closure,
            &locator,
            &decoded_manifest,
            tier,
        )?;
        let verified = verify_typed_v2_history_content(
            &self.store,
            &closure,
            &locator,
            &decoded_manifest,
            tier,
            jumbo_limits,
        )?;
        let _state_lock = self.acquire_state_lock()?;
        let (_, pending_locators_remain) = self
            .generations
            .reconcile_pending_typed_v2_locators(target)?;
        if pending_locators_remain {
            return Err("typed V2 locator recovery remains bounded and must be retried".to_owned());
        }
        let locator_id = self.generations.typed_v2_locator_identity(&locator)?;
        let proposal = self
            .generations
            .propose_typed_v2_history_commit(
                target,
                parents,
                provenance,
                &verified,
                closure_claim,
                locator_id,
            )
            .map_err(|error| error.to_string())?;
        let commit = proposal.identity();
        self.generations
            .stage_typed_v2_locator(target, commit, locator)?;
        #[cfg(test)]
        crate::ir_generation_store::trip_history_test_fault(
            crate::ir_generation_store::HistoryTestFault::AfterTypedV2Locator,
        )?;
        let receipt = match self.generations.admit_typed_v2_history_proposal(
            proposal,
            crate::ir_generation_store::AdmittedHistoryPayloadRoot {
                closure: closure.id(),
            },
            source,
        ) {
            Ok(receipt) => receipt,
            Err(error) => {
                self.generations
                    .reconcile_typed_v2_locator_admission(target, commit)
                    .map_err(|recovery| {
                        format!("{error}; typed V2 locator recovery failed: {recovery}")
                    })?;
                return Err(error);
            }
        };
        self.generations
            .finish_typed_v2_locator_admission(target, commit)?;
        receipt
            .with_gc_pin(std::sync::Arc::new(gc_pin))
            .with_typed_v2_proof(
                verified,
                closure_claim,
                locator_id,
                self.store.root().to_path_buf(),
            )
    }

    /// Publishes an already admitted typed V2 commit while borrowing the live
    /// receipt that owns its exact verifier proof and FileStore GC pin. This
    /// path only rechecks immutable history metadata under the state lock; the
    /// expensive semantic closure scan has already completed during admission.
    pub fn publish_typed_v2_history_ref(
        &self,
        target: &crate::SemanticTargetKey,
        kind: crate::HistoryRefKind,
        name: crate::HistoryRefName,
        expected: Option<crate::HistoryCommitId>,
        admission: &crate::HistoryAdmissionReceipt,
    ) -> Result<crate::HistoryRefUpdateReceipt, String> {
        let proof = admission
            .typed_v2_publication_admission(self.store.root())
            .ok_or_else(|| {
                "typed V2 publication requires a live same-store verifier receipt".to_owned()
            })?;
        let _state_lock = self.acquire_state_lock()?;
        let receipt = self.generations.compare_and_swap_typed_v2_history_ref(
            target,
            kind,
            name,
            expected,
            proof.identity(),
            &proof,
        )?;
        let _ = self.generations.current(target)?;
        Ok(receipt)
    }

    /// Cold-revalidates and publishes a typed V2 commit after process restart,
    /// when no live admission receipt can carry the original GC pin. This
    /// acquires a fresh pin before reading the closure and keeps it through the
    /// durable ref CAS.
    pub fn publish_typed_v2_history_ref_cold(
        &self,
        target: &crate::SemanticTargetKey,
        kind: crate::HistoryRefKind,
        name: crate::HistoryRefName,
        expected: Option<crate::HistoryCommitId>,
        commit_id: crate::HistoryCommitId,
        tier: SemanticTypedPlaneVerificationTierV2,
        jumbo_limits: JumboRopeLimits,
    ) -> Result<crate::HistoryRefUpdateReceipt, String> {
        let gc_pin = self
            .store
            .pin_garbage_collection()
            .map_err(|error| format!("pin cold typed V2 publication: {error:?}"))?;
        let _state_lock = self.acquire_state_lock()?;
        let commit = self.generations.history_commit(target, commit_id)?;
        let claim = commit
            .generation_root()
            .typed_v2_claim()
            .ok_or_else(|| "history commit does not name a typed V2 generation".to_owned())?;
        let locator = self
            .generations
            .typed_v2_locator(target, commit_id, claim.locator())?;
        let manifest = locator.validate()?;
        if manifest.content_root_claim().as_bytes() != claim.content_root_claim().as_bytes()
            || manifest.generation_root_claim().as_bytes()
                != claim.generation_root_claim().as_bytes()
        {
            return Err("typed V2 commit roots differ from its cold manifest".to_owned());
        }
        let closure = self
            .store
            .open_closure_claim(claim.closure())
            .map_err(|error| format!("open cold typed V2 publication closure: {error:?}"))?;
        reopen_typed_v2_history_closure(
            &self.store,
            claim.closure(),
            &closure,
            &locator,
            &manifest,
            tier,
        )?;
        let verified = verify_typed_v2_history_content(
            &self.store,
            &closure,
            &locator,
            &manifest,
            tier,
            jumbo_limits,
        )?;
        if !claim.content_root_claim().matches(verified.content_root())
            || !claim
                .generation_root_claim()
                .matches(verified.generation_root())
        {
            return Err(
                "typed V2 history commit root failed cold publication verification".to_owned(),
            );
        }
        let proof =
            crate::ir_generation_store::TypedV2HistoryPublicationAdmission::from_cold_verification(
                commit_id,
                verified,
                claim.closure(),
                claim.locator(),
                &gc_pin,
            );
        let receipt = self.generations.compare_and_swap_typed_v2_history_ref(
            target, kind, name, expected, commit_id, &proof,
        )?;
        let _ = self.generations.current(target)?;
        Ok(receipt)
    }

    /// Cold-replays one typed V2 commit that is reachable from the exact
    /// supplied named-ref tip. The returned token is minted only after the
    /// manifest, V2 roots, every segment object, all cross-family semantics,
    /// and every referenced jumbo rope closure pass verification.
    pub fn replay_typed_v2_history(
        &self,
        target: &crate::SemanticTargetKey,
        kind: crate::HistoryRefKind,
        name: &crate::HistoryRefName,
        commit_id: crate::HistoryCommitId,
        ancestry: &crate::HistoryRefAncestryProof,
        tier: SemanticTypedPlaneVerificationTierV2,
        jumbo_limits: JumboRopeLimits,
    ) -> Result<crate::TypedV2HistoryReplay, String> {
        let gc_pin = self
            .store
            .pin_garbage_collection()
            .map_err(|error| format!("pin typed V2 history replay: {error:?}"))?;
        let (commit, claim, locator) = {
            let _state_lock = self.acquire_state_lock()?;
            self.validate_history_ref_proof(target, kind, name, commit_id, ancestry)?;
            let commit = self.generations.history_commit(target, commit_id)?;
            let claim = commit
                .generation_root()
                .typed_v2_claim()
                .ok_or_else(|| "history commit does not name a typed V2 generation".to_owned())?;
            let locator = self
                .generations
                .typed_v2_locator(target, commit_id, claim.locator())?;
            (commit, claim, locator)
        };
        let manifest = locator.validate()?;
        if manifest.content_root_claim().as_bytes() != claim.content_root_claim().as_bytes()
            || manifest.generation_root_claim().as_bytes()
                != claim.generation_root_claim().as_bytes()
        {
            return Err("typed V2 commit roots differ from its cold manifest".to_owned());
        }
        let closure = self
            .store
            .open_closure_claim(claim.closure())
            .map_err(|error| format!("open typed V2 history closure: {error:?}"))?;
        reopen_typed_v2_history_closure(
            &self.store,
            claim.closure(),
            &closure,
            &locator,
            &manifest,
            tier,
        )?;
        let verified = verify_typed_v2_history_content(
            &self.store,
            &closure,
            &locator,
            &manifest,
            tier,
            jumbo_limits,
        )?;
        if !claim.content_root_claim().matches(verified.content_root())
            || !claim
                .generation_root_claim()
                .matches(verified.generation_root())
        {
            return Err("typed V2 history commit root failed cold verification".to_owned());
        }
        {
            let _state_lock = self.acquire_state_lock()?;
            self.validate_history_ref_proof(target, kind, name, commit_id, ancestry)?;
        }
        Ok(crate::TypedV2HistoryReplay::new(commit, manifest, verified)
            .with_gc_pin(std::sync::Arc::new(gc_pin)))
    }
}

fn reopen_typed_v2_history_closure(
    store: &FileStore,
    claim: ArtifactClosureClaim,
    closure: &DurableManifest,
    locator: &crate::ir_generation_store::TypedV2HistoryLocator,
    manifest: &SemanticTypedPlaneManifestV2,
    tier: SemanticTypedPlaneVerificationTierV2,
) -> Result<(), String> {
    let mut members: Vec<(UntrustedObjectId, u64, SchemaIdentity)> = Vec::new();
    members
        .try_reserve_exact(locator.segments.len().saturating_add(locator.jumbo.len()))
        .map_err(|_| "typed V2 history closure map allocation failed".to_owned())?;
    for family in manifest.families() {
        for segment in family.segments() {
            let mapped = locator
                .segments
                .binary_search_by(|candidate| {
                    candidate
                        .segment()
                        .as_bytes()
                        .cmp(segment.id_claim().as_bytes())
                })
                .ok()
                .and_then(|index| locator.segments.get(index))
                .copied()
                .ok_or_else(|| "typed V2 history segment object mapping is missing".to_owned())?;
            if mapped.byte_length() != segment.byte_length()
                || mapped.byte_length() > MAX_SEMANTIC_SEGMENT_BYTES as u64
            {
                return Err("typed V2 history segment map exceeds its descriptor bounds".to_owned());
            }
            members.push((
                mapped.object(),
                mapped.byte_length(),
                crate::ProducedSemanticObjectKind::Segment.schema_identity(),
            ));
        }
    }
    for object in &locator.jumbo {
        let maximum = match object.kind() {
            JumboRopeObjectKind::Leaf => JUMBO_ROPE_MAX_LEAF_BYTES as u64,
            JumboRopeObjectKind::Interior => MAX_JUMBO_INTERIOR_BYTES,
        };
        if object.byte_length() == 0 || object.byte_length() > maximum {
            return Err("typed V2 history rope map exceeds its object-kind bounds".to_owned());
        }
        members.push((
            object.object(),
            object.byte_length(),
            jumbo_schema_identity(object.kind()),
        ));
    }
    members.sort_unstable_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    let mut unique_members: Vec<(UntrustedObjectId, u64, SchemaIdentity)> = Vec::new();
    unique_members
        .try_reserve_exact(members.len())
        .map_err(|_| "typed V2 history closure set allocation failed".to_owned())?;
    for member in members {
        if let Some((previous, length, schema)) = unique_members.last() {
            if previous.as_bytes() == member.0.as_bytes() {
                if *length != member.1 || *schema != member.2 {
                    return Err(
                        "one typed V2 FileStore object has conflicting mapped schema or length"
                            .to_owned(),
                    );
                }
                continue;
            }
        }
        unique_members.push(member);
    }
    if unique_members.len() > MAX_TYPED_V2_CLOSURE_OBJECTS
        || closure.object_count() != u64::try_from(unique_members.len()).map_err(display_error)?
    {
        return Err("typed V2 history closure is not the exact bounded locator closure".to_owned());
    }
    let total_bytes = unique_members
        .iter()
        .try_fold(0_u64, |total, (_, length, _)| total.checked_add(*length))
        .ok_or_else(|| "typed V2 history closure byte count overflows".to_owned())?;
    if total_bytes > verification_byte_limit(tier) {
        return Err("typed V2 history closure exceeds its verification tier".to_owned());
    }
    for (object, length, schema) in &unique_members {
        let member = closure
            .admit_claim(*object)
            .map_err(|error| format!("check typed V2 history closure membership: {error:?}"))?
            .ok_or_else(|| "typed V2 history closure omits a mapped object".to_owned())?;
        if member.as_bytes() != object.as_bytes()
            || open_history_object_reader(store, closure, *object, *length, *length, *schema)?
                .is_none()
        {
            return Err("typed V2 history closure member differs from its locator map".to_owned());
        }
    }
    #[cfg(test)]
    TYPED_V2_CLOSURE_REOPEN_COUNT.with(|count| count.set(count.get().saturating_add(1)));
    store
        .reopen_stored_closure(
            claim,
            ArtifactBudget::new(
                1,
                MAX_TYPED_V2_CLOSURE_OBJECTS,
                total_bytes.max(1),
                IO_BUFFER_BYTES,
                1,
            ),
        )
        .map_err(|error| format!("reopen typed V2 history closure: {error:?}"))?;
    Ok(())
}

fn verification_byte_limit(tier: SemanticTypedPlaneVerificationTierV2) -> u64 {
    match tier {
        SemanticTypedPlaneVerificationTierV2::Standard => MAX_TYPED_V2_STANDARD_BYTES,
        SemanticTypedPlaneVerificationTierV2::LargePackage => MAX_TYPED_V2_LARGE_BYTES,
    }
}

fn verify_typed_v2_history_content(
    store: &FileStore,
    closure: &DurableManifest,
    locator: &crate::ir_generation_store::TypedV2HistoryLocator,
    manifest: &SemanticTypedPlaneManifestV2,
    tier: SemanticTypedPlaneVerificationTierV2,
    jumbo_limits: JumboRopeLimits,
) -> Result<VerifiedTypedPlaneContentV2, String> {
    let total_bytes = manifest
        .families()
        .iter()
        .flat_map(|family| family.segments())
        .try_fold(0_u64, |total, segment| {
            total.checked_add(segment.byte_length())
        })
        .ok_or_else(|| "typed V2 history payload byte count overflows".to_owned())?;
    if total_bytes > verification_byte_limit(tier) {
        return Err("typed V2 history payload closure exceeds its verification tier".to_owned());
    }
    let mut payloads = Vec::new();
    payloads
        .try_reserve_exact(locator.segments.len())
        .map_err(|_| "typed V2 history payload list allocation failed".to_owned())?;
    for family in manifest.families() {
        for segment in family.segments() {
            let mapped = locator
                .segments
                .binary_search_by(|candidate| {
                    candidate
                        .segment()
                        .as_bytes()
                        .cmp(segment.id_claim().as_bytes())
                })
                .ok()
                .and_then(|index| locator.segments.get(index))
                .copied()
                .ok_or_else(|| "typed V2 history segment object mapping is missing".to_owned())?;
            if mapped.byte_length() != segment.byte_length() {
                return Err("typed V2 history segment mapping has the wrong length".to_owned());
            }
            let payload = read_history_object_payload(
                store,
                closure,
                mapped.object(),
                mapped.byte_length(),
                MAX_SEMANTIC_SEGMENT_BYTES as u64,
                crate::ProducedSemanticObjectKind::Segment.schema_identity(),
            )?
            .ok_or_else(|| "typed V2 history segment object is missing".to_owned())?;
            payloads.push(payload);
        }
    }
    let mut ordered_payloads = Vec::new();
    ordered_payloads
        .try_reserve_exact(payloads.len())
        .map_err(|_| "typed V2 ordered payload list allocation failed".to_owned())?;
    ordered_payloads.extend(payloads.iter().map(Vec::as_slice));
    let mut jumbo_source = HistoryJumboSource {
        store,
        closure,
        mappings: &locator.jumbo,
        used: {
            let mut used = Vec::new();
            used.try_reserve_exact(locator.jumbo.len())
                .map_err(|_| "typed V2 history rope-use map allocation failed".to_owned())?;
            used.resize(locator.jumbo.len(), false);
            used
        },
    };
    let verified = verify_typed_plane_content_v2_with_jumbo_source(
        manifest,
        &ordered_payloads,
        tier,
        jumbo_limits,
        &mut jumbo_source,
    )
    .map_err(|error| format!("verify typed V2 history content: {error}"))?;
    if jumbo_source.used.iter().any(|used| !used) {
        return Err("typed V2 history locator contains an unreferenced rope object".to_owned());
    }
    Ok(verified)
}

fn read_history_object_payload(
    store: &FileStore,
    closure: &DurableManifest,
    object_claim: UntrustedObjectId,
    expected_length: u64,
    maximum_length: u64,
    expected_schema: SchemaIdentity,
) -> Result<Option<Vec<u8>>, String> {
    let Some((mut reader, admitted_id)) = open_history_object_reader(
        store,
        closure,
        object_claim,
        expected_length,
        maximum_length,
        expected_schema,
    )?
    else {
        return Ok(None);
    };
    let capacity = usize::try_from(expected_length)
        .map_err(|_| "typed V2 history object length exceeds address space".to_owned())?;
    let mut payload = Vec::new();
    payload
        .try_reserve_exact(capacity)
        .map_err(|_| "typed V2 history object allocation failed".to_owned())?;
    payload.resize(capacity, 0);
    let mut offset = 0_u64;
    while offset < expected_length {
        let available = expected_length - offset;
        let take = usize::try_from(available.min(IO_BUFFER_BYTES as u64))
            .map_err(|_| "typed V2 history read length exceeds address space".to_owned())?;
        let start = usize::try_from(offset)
            .map_err(|_| "typed V2 history offset exceeds address space".to_owned())?;
        let end = start
            .checked_add(take)
            .ok_or_else(|| "typed V2 history range overflows".to_owned())?;
        let read = reader
            .read_payload_range(offset, &mut payload[start..end])
            .map_err(|error| format!("read typed V2 history payload: {error:?}"))?;
        if read != take {
            return Err("typed V2 history object ended before its exact length".to_owned());
        }
        offset = offset
            .checked_add(u64::try_from(read).map_err(display_error)?)
            .ok_or_else(|| "typed V2 history offset overflows".to_owned())?;
    }
    if reader.id() != admitted_id {
        return Err("typed V2 history object changed after closure admission".to_owned());
    }
    Ok(Some(payload))
}

fn open_history_object_reader(
    store: &FileStore,
    closure: &DurableManifest,
    object_claim: UntrustedObjectId,
    expected_length: u64,
    maximum_length: u64,
    expected_schema: SchemaIdentity,
) -> Result<Option<(ArtifactObjectReader, backend_store::ObjectId)>, String> {
    let admitted_id = closure
        .admit_claim(object_claim)
        .map_err(|error| format!("check typed V2 history closure membership: {error:?}"))?;
    let Some(admitted_id) = admitted_id else {
        return Ok(None);
    };
    let sink = store.artifact_sink(ArtifactBudget::new(
        1,
        1,
        maximum_length.max(1),
        IO_BUFFER_BYTES,
        1,
    ));
    let Some(reader) = sink
        .open_object_limited(object_claim, maximum_length)
        .map_err(|error| format!("open typed V2 history object: {error:?}"))?
    else {
        return Ok(None);
    };
    if reader.id() != admitted_id
        || reader.id().as_bytes() != object_claim.as_bytes()
        || reader.schema() != expected_schema
        || reader.payload_len() != expected_length
        || reader.payload_len() > maximum_length
    {
        return Err("typed V2 history object envelope differs from its locator".to_owned());
    }
    Ok(Some((reader, admitted_id)))
}

struct HistoryJumboSource<'a> {
    store: &'a FileStore,
    closure: &'a DurableManifest,
    mappings: &'a [HistoryTypedV2JumboObject],
    used: Vec<bool>,
}

impl HistoryJumboSource<'_> {
    fn mapping_index(&self, id: JumboRopeObjectId, kind: JumboRopeObjectKind) -> Option<usize> {
        jumbo_mapping_index(self.mappings, id, kind)
    }

    fn open(
        &self,
        mapping: HistoryTypedV2JumboObject,
        expected_kind: JumboRopeObjectKind,
        maximum_length: u64,
    ) -> Result<Option<(ArtifactObjectReader, backend_store::ObjectId)>, String> {
        if mapping.kind() != expected_kind {
            return Err("typed V2 history rope map uses the wrong object kind".to_owned());
        }
        open_history_object_reader(
            self.store,
            self.closure,
            mapping.object(),
            mapping.byte_length(),
            maximum_length,
            jumbo_schema_identity(expected_kind),
        )
    }
}

impl JumboRopeObjectSource for HistoryJumboSource<'_> {
    type Error = String;

    fn read_leaf(
        &mut self,
        id: JumboRopeObjectId,
        output: &mut [u8; JUMBO_ROPE_MAX_LEAF_BYTES],
    ) -> Result<Option<usize>, Self::Error> {
        let Some(index) = self.mapping_index(id, JumboRopeObjectKind::Leaf) else {
            return Ok(None);
        };
        self.used[index] = true;
        let mapping = self.mappings[index];
        let Some((mut reader, admitted_id)) = self.open(
            mapping,
            JumboRopeObjectKind::Leaf,
            JUMBO_ROPE_MAX_LEAF_BYTES as u64,
        )?
        else {
            return Ok(None);
        };
        let length = usize::try_from(mapping.byte_length())
            .map_err(|_| "typed V2 history rope leaf length exceeds address space".to_owned())?;
        if length == 0 || length > output.len() || reader.id() != admitted_id {
            return Err("typed V2 history rope leaf exceeds its bounded buffer".to_owned());
        }
        let mut offset = 0_usize;
        while offset < length {
            let take = (length - offset).min(IO_BUFFER_BYTES);
            let read = reader
                .read_payload_range(offset as u64, &mut output[offset..offset + take])
                .map_err(|error| format!("read typed V2 history rope leaf: {error:?}"))?;
            if read != take {
                return Err("typed V2 history rope leaf ended before its exact length".to_owned());
            }
            offset += read;
        }
        Ok(Some(length))
    }

    fn read_interior(
        &mut self,
        id: JumboRopeObjectId,
    ) -> Result<Option<[u8; ROPE_NODE_WIRE_BYTES]>, Self::Error> {
        let Some(index) = self.mapping_index(id, JumboRopeObjectKind::Interior) else {
            return Ok(None);
        };
        self.used[index] = true;
        let mapping = self.mappings[index];
        let Some((mut reader, admitted_id)) = self.open(
            mapping,
            JumboRopeObjectKind::Interior,
            MAX_JUMBO_INTERIOR_BYTES,
        )?
        else {
            return Ok(None);
        };
        if mapping.byte_length() != MAX_JUMBO_INTERIOR_BYTES || reader.id() != admitted_id {
            return Err("typed V2 history rope interior has the wrong fixed length".to_owned());
        }
        let mut output = [0_u8; ROPE_NODE_WIRE_BYTES];
        let read = reader
            .read_payload_range(0, &mut output)
            .map_err(|error| format!("read typed V2 history rope interior: {error:?}"))?;
        if read != output.len() {
            return Err("typed V2 history rope interior is truncated".to_owned());
        }
        Ok(Some(output))
    }
}

fn jumbo_mapping_index(
    mappings: &[HistoryTypedV2JumboObject],
    id: JumboRopeObjectId,
    kind: JumboRopeObjectKind,
) -> Option<usize> {
    let order = jumbo_map_order_parts(kind, *id.as_bytes());
    mappings
        .binary_search_by(|candidate| jumbo_map_order(*candidate).cmp(&order))
        .ok()
}

fn jumbo_schema_identity(kind: JumboRopeObjectKind) -> SchemaIdentity {
    match kind {
        JumboRopeObjectKind::Leaf => crate::ProducedSemanticObjectKind::JumboLeaf.schema_identity(),
        JumboRopeObjectKind::Interior => {
            crate::ProducedSemanticObjectKind::JumboInterior.schema_identity()
        }
    }
}

fn jumbo_map_order(object: HistoryTypedV2JumboObject) -> (u8, [u8; 32]) {
    jumbo_map_order_parts(object.kind(), object.id())
}

fn jumbo_map_order_parts(kind: JumboRopeObjectKind, id: [u8; 32]) -> (u8, [u8; 32]) {
    (
        match kind {
            JumboRopeObjectKind::Leaf => 0,
            JumboRopeObjectKind::Interior => 1,
        },
        id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::ir::{
        ImageProvenance, JumboRopeLimits, JumboRopeNode, JumboRopeObjectId, JumboRopeObjectKind,
        JumboRopeObjectSink, JumboRopeWriteReceipt, JumboValueContext, JumboValueEncoding,
        JumboValueFamily, LanguageProfile, RustEdition, SemanticBuildIdentity,
        SemanticImageAuthority, SemanticImageFacts, SemanticInputClaimV2, SemanticIrPlane,
        SemanticTypedPlaneFamilyDescriptorV2, SemanticTypedPlaneManifestV2,
        SemanticTypedPlaneSegmentClaimV2, UntrustedSemanticContentRootV2,
        UntrustedSemanticGenerationRootV2, UntrustedSemanticSegmentId, write_jumbo_value,
    };
    use backend_store::{
        ArtifactClosureClaim, ArtifactObjectClaim, StreamingClosureBudget, TypedObject,
    };
    use backend_version::{Coverage, ObjectKey, Schema, ScopeRoot, Stage};
    use std::convert::Infallible;
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
                "backend-typed-v2-history-cold-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create history fixture directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct TestPayload;

    impl Schema for TestPayload {
        const DOMAIN: u8 = 0x7d;
        const TYPE: u16 = 0x9001;
        const VERSION: u8 = 1;
        type Value = [u8];

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    #[derive(Default)]
    struct CapturedJumboIds {
        leaves: Vec<JumboRopeObjectId>,
        interiors: Vec<JumboRopeObjectId>,
    }

    impl JumboRopeObjectSink for CapturedJumboIds {
        type Error = Infallible;

        fn write_leaf(
            &mut self,
            leaf: backend_semantic::ir::JumboRopeLeafRef<'_>,
        ) -> Result<(), Self::Error> {
            self.leaves.push(leaf.id());
            Ok(())
        }

        fn write_interior(&mut self, node: &JumboRopeNode) -> Result<(), Self::Error> {
            self.interiors.push(node.id());
            Ok(())
        }
    }

    fn empty_manifest() -> SemanticTypedPlaneManifestV2 {
        typed_manifest(false)
    }

    fn one_segment_manifest() -> SemanticTypedPlaneManifestV2 {
        typed_manifest(true)
    }

    fn typed_manifest(one_segment: bool) -> SemanticTypedPlaneManifestV2 {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let build = SemanticBuildIdentity::new(
            [1; 32],
            [2; 32],
            profile,
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
            [6; 32],
        );
        let families = [
            SemanticIrPlane::Core,
            SemanticIrPlane::Types,
            SemanticIrPlane::Relations,
            SemanticIrPlane::Occurrences,
            SemanticIrPlane::Documentation,
            SemanticIrPlane::SourceProvenance,
            SemanticIrPlane::LanguageExtensions(profile),
        ]
        .map(|family| {
            if one_segment && family == SemanticIrPlane::Core {
                let segment = SemanticTypedPlaneSegmentClaimV2::from_untrusted_claims(
                    [1; 32],
                    [1; 32],
                    1,
                    1,
                    UntrustedSemanticSegmentId::from_raw([2; 32]),
                )
                .expect("one canonical segment claim");
                SemanticTypedPlaneFamilyDescriptorV2::from_untrusted_claims(
                    family,
                    1,
                    vec![segment],
                )
                .expect("one-row core family")
            } else {
                SemanticTypedPlaneFamilyDescriptorV2::from_untrusted_claims(family, 0, Vec::new())
                    .expect("empty family")
            }
        });
        SemanticTypedPlaneManifestV2::from_untrusted_claims(
            build,
            SemanticImageFacts {
                authority: SemanticImageAuthority::Shared,
                provenance: ImageProvenance::Unavailable,
            },
            SemanticInputClaimV2::from_untrusted_claims(
                [7; 32],
                ScopeRoot::from_bytes([8; 32]),
                Coverage::Complete,
            ),
            UntrustedSemanticContentRootV2::from_wire_claim([9; 32]),
            UntrustedSemanticGenerationRootV2::from_wire_claim([10; 32]),
            families,
        )
        .expect("structurally canonical untrusted manifest")
    }

    fn one_object_closure(
        store: &FileStore,
        schema: SchemaIdentity,
        payload: &[u8],
    ) -> (ArtifactClosureClaim, UntrustedObjectId) {
        let typed =
            TypedObject::from_value(&ObjectKey::<TestPayload>::from_value(payload), payload);
        assert_eq!(typed.schema(), schema);
        let claim = ArtifactObjectClaim::new(
            typed.schema(),
            *typed.key(),
            *typed.version(),
            u64::try_from(typed.bytes().len()).expect("bounded test payload length"),
        )
        .with_object_id(UntrustedObjectId::from_bytes(*typed.id().as_bytes()));
        let mut builder = store
            .begin_streaming_closure(StreamingClosureBudget::new(
                1,
                u64::try_from(payload.len()).expect("bounded test payload length"),
                payload.len(),
                payload.len(),
                1,
                4096,
            ))
            .expect("begin one-object closure");
        let mut stream = builder
            .begin_object(claim)
            .expect("begin exact typed test object");
        stream.write(payload).expect("write exact test payload");
        let object = stream.finish().expect("admit exact test object");
        let closure = builder.seal().expect("seal one-object closure");
        (
            ArtifactClosureClaim::from_id(closure.closure()),
            UntrustedObjectId::from_bytes(*object.as_bytes()),
        )
    }

    fn empty_closure(store: &FileStore) -> DurableManifest {
        let closure = store
            .begin_streaming_closure(StreamingClosureBudget::new(
                1,
                1,
                1,
                IO_BUFFER_BYTES,
                1,
                4096,
            ))
            .expect("begin empty closure")
            .seal()
            .expect("seal empty closure");
        store
            .open_closure_claim(ArtifactClosureClaim::from_id(closure.closure()))
            .expect("reopen empty closure")
    }

    fn capture_rope_ids(bytes: &[u8]) -> CapturedJumboIds {
        let mut sink = CapturedJumboIds::default();
        let context = JumboValueContext::new(
            [0x35; 32],
            JumboValueFamily::Documentation,
            0,
            JumboValueEncoding::Bytes,
        );
        let _receipt: JumboRopeWriteReceipt =
            write_jumbo_value(context, bytes, JumboRopeLimits::default(), &mut sink)
                .expect("write canonical jumbo fixture");
        sink
    }

    #[test]
    fn cold_v2_rejects_a_partial_segment_closure() {
        let directory = TestDirectory::create();
        let store = FileStore::open(&directory.0, 1024 * 1024).expect("open test FileStore");
        let closure = empty_closure(&store);
        let manifest = one_segment_manifest();
        let locator = super::super::ir_generation_store::TypedV2HistoryLocator {
            manifest: manifest.canonical_bytes().expect("encode manifest"),
            segments: vec![HistoryTypedV2SegmentObject::new(
                UntrustedSemanticSegmentId::from_raw([2; 32]),
                UntrustedObjectId::from_bytes([3; 32]),
                1,
            )],
            jumbo: Vec::new(),
        };
        locator.validate().expect("canonical locator map");
        let error = reopen_typed_v2_history_closure(
            &store,
            ArtifactClosureClaim::from_id(closure.id()),
            &closure,
            &locator,
            &manifest,
            SemanticTypedPlaneVerificationTierV2::Standard,
        )
        .expect_err("a required segment cannot be omitted from its closure");
        assert!(error.contains("exact bounded locator closure"));
    }

    #[test]
    fn cold_v2_rejects_extra_closure_members_and_wrong_jumbo_schema() {
        let directory = TestDirectory::create();
        let store = FileStore::open(&directory.0, 1024 * 1024).expect("open test FileStore");
        let empty = empty_closure(&store);
        let (extra_claim, _) = one_object_closure(&store, test_payload_schema(), b"extra");
        let empty_manifest = empty_manifest();
        let empty_locator = super::super::ir_generation_store::TypedV2HistoryLocator {
            manifest: empty_manifest.canonical_bytes().expect("encode manifest"),
            segments: Vec::new(),
            jumbo: Vec::new(),
        };
        let extra = store
            .open_closure_claim(extra_claim)
            .expect("reopen extra-member closure");
        let error = reopen_typed_v2_history_closure(
            &store,
            extra_claim,
            &extra,
            &empty_locator,
            &empty_manifest,
            SemanticTypedPlaneVerificationTierV2::Standard,
        )
        .expect_err("an unclaimed closure member is not part of V2 history");
        assert!(error.contains("exact bounded locator closure"));

        let ids = capture_rope_ids(&vec![0x41; 300 * 1024]);
        let interior_id = *ids.interiors.first().expect("multi-leaf rope has interior");
        let (wrong_schema_claim, object_id) =
            one_object_closure(&store, test_payload_schema(), &[0x52; 142]);
        let wrong_schema = store
            .open_closure_claim(wrong_schema_claim)
            .expect("reopen wrong-schema closure");
        let wrong_kind_locator = super::super::ir_generation_store::TypedV2HistoryLocator {
            manifest: empty_manifest.canonical_bytes().expect("encode manifest"),
            segments: Vec::new(),
            jumbo: vec![HistoryTypedV2JumboObject::new(
                interior_id,
                JumboRopeObjectKind::Interior,
                object_id,
                142,
            )],
        };
        wrong_kind_locator
            .validate()
            .expect("interior map has a canonical fixed length");
        let error = reopen_typed_v2_history_closure(
            &store,
            wrong_schema_claim,
            &wrong_schema,
            &wrong_kind_locator,
            &empty_manifest,
            SemanticTypedPlaneVerificationTierV2::Standard,
        )
        .expect_err("interior objects require their exact distinct FileStore schema");
        assert!(error.contains("envelope differs"));
    }

    #[test]
    fn rope_source_does_not_resolve_an_interior_id_as_a_leaf() {
        let ids = capture_rope_ids(&vec![0x63; 300 * 1024]);
        let interior_id = *ids.interiors.first().expect("multi-leaf rope has interior");
        let wrong_kind = [HistoryTypedV2JumboObject::new(
            interior_id,
            JumboRopeObjectKind::Leaf,
            UntrustedObjectId::from_bytes([4; 32]),
            1,
        )];
        assert_eq!(
            jumbo_mapping_index(&wrong_kind, interior_id, JumboRopeObjectKind::Leaf),
            Some(0)
        );
        assert_eq!(
            jumbo_mapping_index(&wrong_kind, interior_id, JumboRopeObjectKind::Interior),
            None,
            "the exact ID with the wrong semantic kind cannot satisfy an interior read"
        );
    }

    fn test_payload_schema() -> SchemaIdentity {
        SchemaIdentity::new(TestPayload::DOMAIN, TestPayload::TYPE, TestPayload::VERSION)
    }
}
