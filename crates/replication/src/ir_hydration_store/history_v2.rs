//! FileStore-backed cold replay for typed V2 history commits.

use super::{FileSemanticRangeStore, IO_BUFFER_BYTES, display_error, display_io};

use crate::{HistoryTypedV2JumboObject, HistoryTypedV2SegmentObject};
use backend_semantic::ir::{
    JUMBO_ROPE_MAX_LEAF_BYTES, JumboRopeLimits, JumboRopeObjectId, JumboRopeObjectKind,
    JumboRopeObjectSource, MAX_SEMANTIC_SEGMENT_BYTES, ROPE_NODE_WIRE_BYTES,
    SemanticTypedPlaneManifestV2, SemanticTypedPlaneSegmentClaimV2,
    SemanticTypedPlaneVerificationTierV2, TypedPlaneSegmentSourceV2, VerifiedTypedPlaneContentV2,
    verify_typed_plane_content_v2_with_jumbo_segment_source,
};
use backend_store::{
    ArtifactClosureClaim, DurableManifest, FileStore, ObjectId, UntrustedObjectId,
};
use backend_version::SchemaIdentity;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_TYPED_V2_CLOSURE_OBJECTS: usize = 200_000;
const MAX_TYPED_V2_STANDARD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TYPED_V2_LARGE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_JUMBO_INTERIOR_BYTES: u64 = ROPE_NODE_WIRE_BYTES as u64;

#[cfg(test)]
thread_local! {
    static TYPED_V2_CLOSURE_REOPEN_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static TYPED_V2_COLD_PUBLICATION_HOOK: std::cell::RefCell<Option<std::sync::Arc<std::sync::Barrier>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn reset_typed_v2_closure_reopen_count() {
    TYPED_V2_CLOSURE_REOPEN_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(super) fn typed_v2_closure_reopen_count() -> usize {
    TYPED_V2_CLOSURE_REOPEN_COUNT.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(super) fn set_typed_v2_cold_publication_hook(hook: Option<std::sync::Arc<std::sync::Barrier>>) {
    TYPED_V2_COLD_PUBLICATION_HOOK.with(|current| *current.borrow_mut() = hook);
}

#[cfg(test)]
fn wait_at_typed_v2_cold_publication_hook() {
    let hook = TYPED_V2_COLD_PUBLICATION_HOOK.with(|current| current.borrow().clone());
    if let Some(hook) = hook {
        hook.wait();
        hook.wait();
        set_typed_v2_cold_publication_hook(None);
    }
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
        let spool = spool_typed_v2_history_closure(
            &self.store,
            closure_claim,
            &closure,
            &locator,
            &decoded_manifest,
            tier,
        )?;
        let verified = verify_typed_v2_history_content(
            &spool,
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
        let snapshot: crate::ir_generation_store::TypedV2HistoryPublicationSnapshot = {
            let _state_lock = self.acquire_state_lock()?;
            self.generations
                .typed_v2_publication_snapshot(target, commit_id)?
        };
        let claim = snapshot.claim();
        let locator = snapshot.locator();
        let manifest = locator.validate()?;
        #[cfg(test)]
        wait_at_typed_v2_cold_publication_hook();
        let closure = self
            .store
            .open_closure_claim(claim.closure())
            .map_err(|error| format!("open cold typed V2 publication closure: {error:?}"))?;
        let spool = spool_typed_v2_history_closure(
            &self.store,
            claim.closure(),
            &closure,
            &locator,
            &manifest,
            tier,
        )?;
        let verified =
            verify_typed_v2_history_content(&spool, &locator, &manifest, tier, jumbo_limits)?;
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
                snapshot.identity(),
                verified,
                claim.closure(),
                claim.locator(),
                &gc_pin,
            );
        let _state_lock = self.acquire_state_lock()?;
        self.generations
            .revalidate_typed_v2_publication_snapshot(target, &snapshot)?;
        let receipt = self.generations.compare_and_swap_typed_v2_history_ref(
            target,
            kind,
            name,
            expected,
            snapshot.identity(),
            &proof,
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
        let spool = spool_typed_v2_history_closure(
            &self.store,
            claim.closure(),
            &closure,
            &locator,
            &manifest,
            tier,
        )?;
        let verified =
            verify_typed_v2_history_content(&spool, &locator, &manifest, tier, jumbo_limits)?;
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

fn spool_typed_v2_history_closure(
    store: &FileStore,
    _claim: ArtifactClosureClaim,
    closure: &DurableManifest,
    locator: &crate::ir_generation_store::TypedV2HistoryLocator,
    manifest: &SemanticTypedPlaneManifestV2,
    tier: SemanticTypedPlaneVerificationTierV2,
) -> Result<TypedV2HistorySpool, String> {
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
    let mut spool = TypedV2HistorySpool::create()?;
    spool
        .members
        .try_reserve_exact(unique_members.len())
        .map_err(|_| "typed V2 history spool index allocation failed".to_owned())?;
    for (object_claim, length, schema) in &unique_members {
        let member = closure
            .admit_claim(*object_claim)
            .map_err(|error| format!("check typed V2 history closure membership: {error:?}"))?
            .ok_or_else(|| "typed V2 history closure omits a mapped object".to_owned())?;
        if member.as_bytes() != object_claim.as_bytes() {
            return Err("typed V2 history closure member differs from its locator map".to_owned());
        }
        let (offset, admitted_id) =
            spool.append_tentative_payload(store, *object_claim, *length, *length, *schema)?;
        spool.members.push(TypedV2SpoolMember {
            id: admitted_id,
            byte_length: *length,
            schema: *schema,
            offset,
        });
    }
    spool
        .file
        .flush()
        .map_err(|error| format!("flush typed V2 history spool: {error}"))?;
    if spool.bytes_written != total_bytes || spool.members.len() != unique_members.len() {
        return Err("typed V2 history spool differs from its exact closure accounting".to_owned());
    }
    #[cfg(test)]
    TYPED_V2_CLOSURE_REOPEN_COUNT.with(|count| count.set(count.get().saturating_add(1)));
    Ok(spool)
}

fn verification_byte_limit(tier: SemanticTypedPlaneVerificationTierV2) -> u64 {
    match tier {
        SemanticTypedPlaneVerificationTierV2::Standard => MAX_TYPED_V2_STANDARD_BYTES,
        SemanticTypedPlaneVerificationTierV2::LargePackage => MAX_TYPED_V2_LARGE_BYTES,
    }
}

static TYPED_V2_SPOOL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Private bounded-disk backing for the exact locator closure. The spool is
/// capped by the selected verification tier, unlinked immediately on Unix,
/// and delete-on-close on Windows. Other platforms retain a create-new temp
/// path and use best-effort ordinary-drop cleanup.
struct TypedV2HistorySpool {
    path: Option<PathBuf>,
    file: File,
    members: Vec<TypedV2SpoolMember>,
    bytes_written: u64,
}

#[derive(Clone, Copy)]
struct TypedV2SpoolMember {
    id: ObjectId,
    byte_length: u64,
    schema: SchemaIdentity,
    offset: u64,
}

impl TypedV2HistorySpool {
    fn create() -> Result<Self, String> {
        for _ in 0..64 {
            let nonce = TYPED_V2_SPOOL_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "backend-typed-v2-history-{}-{nonce}.spool",
                std::process::id()
            ));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            #[cfg(windows)]
            options.custom_flags(WINDOWS_FILE_FLAG_DELETE_ON_CLOSE);
            match options.open(&path) {
                Ok(file) => {
                    #[cfg(unix)]
                    {
                        if let Err(error) = std::fs::remove_file(&path) {
                            drop(file);
                            let _ = std::fs::remove_file(&path);
                            return Err(format!(
                                "unlink private typed V2 history spool after opening: {error}"
                            ));
                        }
                        return Ok(Self {
                            path: None,
                            file,
                            members: Vec::new(),
                            bytes_written: 0,
                        });
                    }
                    #[cfg(windows)]
                    {
                        return Ok(Self {
                            path: None,
                            file,
                            members: Vec::new(),
                            bytes_written: 0,
                        });
                    }
                    #[cfg(not(any(unix, windows)))]
                    {
                        return Ok(Self {
                            path: Some(path),
                            file,
                            members: Vec::new(),
                            bytes_written: 0,
                        });
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("create typed V2 history spool: {error}")),
            }
        }
        Err("could not reserve a unique typed V2 history spool path".to_owned())
    }

    fn append_tentative_payload(
        &mut self,
        store: &FileStore,
        object_claim: UntrustedObjectId,
        expected_length: u64,
        maximum_length: u64,
        expected_schema: SchemaIdentity,
    ) -> Result<(u64, ObjectId), String> {
        let start = self.bytes_written;
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(|error| format!("seek typed V2 history spool append point: {error}"))?;
        let mut staged_length = 0_u64;
        let streamed =
            store.stream_tentative_object_payload(object_claim, maximum_length, |chunk| {
                self.file
                    .write_all(chunk)
                    .map_err(|error| backend_store::StoreError::Io(error.to_string()))?;
                let advance =
                    u64::try_from(chunk.len()).map_err(|_| backend_store::StoreError::Bounds)?;
                staged_length = staged_length
                    .checked_add(advance)
                    .ok_or(backend_store::StoreError::Bounds)?;
                Ok(())
            });
        let envelope = match streamed {
            Ok(Some(envelope)) => envelope,
            Ok(None) => {
                self.rollback_tentative_tail(start)?;
                return Err("typed V2 history closure member is missing from FileStore".to_owned());
            }
            Err(error) => {
                let rollback = self.rollback_tentative_tail(start);
                if let Err(rollback_error) = rollback {
                    return Err(format!(
                        "typed V2 object verification failed ({error:?}) and provisional spool rollback failed ({rollback_error})"
                    ));
                }
                return Err(format!("verify typed V2 history object: {error:?}"));
            }
        };
        let admitted_id = envelope.id();
        if admitted_id.as_bytes() != object_claim.as_bytes()
            || envelope.schema() != expected_schema
            || envelope.payload_len() != expected_length
            || staged_length != expected_length
        {
            self.rollback_tentative_tail(start)?;
            return Err("typed V2 history object envelope differs from its locator".to_owned());
        }
        let Some(committed_end) = start.checked_add(staged_length) else {
            self.rollback_tentative_tail(start)?;
            return Err("typed V2 history spool size overflows".to_owned());
        };
        self.bytes_written = committed_end;
        Ok((start, admitted_id))
    }

    fn rollback_tentative_tail(&mut self, start: u64) -> Result<(), String> {
        self.file
            .set_len(start)
            .map_err(|error| format!("truncate provisional typed V2 history spool: {error}"))?;
        self.file
            .seek(SeekFrom::Start(start))
            .map_err(|error| format!("seek provisional typed V2 history spool: {error}"))?;
        self.bytes_written = start;
        Ok(())
    }
    fn member(&self, object: UntrustedObjectId) -> Option<TypedV2SpoolMember> {
        self.members
            .binary_search_by(|member| member.id.as_bytes().cmp(object.as_bytes()))
            .ok()
            .and_then(|index| self.members.get(index).copied())
    }

    fn clone_reader(&self) -> Result<File, String> {
        self.file
            .try_clone()
            .map_err(|error| format!("clone typed V2 history spool handle: {error}"))
    }
}

impl Drop for TypedV2HistorySpool {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(windows)]
const WINDOWS_FILE_FLAG_DELETE_ON_CLOSE: i32 = 0x0400_0000;

/// Private capability constructed only after exact closure membership,
/// schema, object identity, and payload length were checked and spooled.
#[derive(Clone, Copy)]
struct AdmittedTypedV2HistorySegment {
    claim: SemanticTypedPlaneSegmentClaimV2,
    object: ObjectId,
    byte_length: u64,
    spool_offset: u64,
}

struct TypedV2SpoolSegmentSource {
    file: File,
    segments: Vec<AdmittedTypedV2HistorySegment>,
    buffer: Vec<u8>,
}

impl TypedV2SpoolSegmentSource {
    fn new(
        spool: &TypedV2HistorySpool,
        locator: &crate::ir_generation_store::TypedV2HistoryLocator,
        manifest: &SemanticTypedPlaneManifestV2,
    ) -> Result<Self, String> {
        let segment_count = manifest
            .families()
            .iter()
            .try_fold(0_usize, |count, family| {
                count.checked_add(family.segments().len())
            })
            .ok_or_else(|| "typed V2 history segment count overflows".to_owned())?;
        let mut segments = Vec::new();
        segments
            .try_reserve_exact(segment_count)
            .map_err(|_| "typed V2 admitted segment map allocation failed".to_owned())?;
        for family in manifest.families() {
            for claim in family.segments() {
                let mapped = locator
                    .segments
                    .binary_search_by(|candidate| {
                        candidate
                            .segment()
                            .as_bytes()
                            .cmp(claim.id_claim().as_bytes())
                    })
                    .ok()
                    .and_then(|index| locator.segments.get(index))
                    .copied()
                    .ok_or_else(|| "typed V2 admitted segment mapping disappeared".to_owned())?;
                let member = spool.member(mapped.object()).ok_or_else(|| {
                    "typed V2 segment is absent from the verified spool".to_owned()
                })?;
                if member.schema != crate::ProducedSemanticObjectKind::Segment.schema_identity()
                    || member.byte_length != claim.byte_length()
                    || member.byte_length != mapped.byte_length()
                {
                    return Err(
                        "typed V2 admitted segment differs from its spool envelope".to_owned()
                    );
                }
                segments.push(AdmittedTypedV2HistorySegment {
                    claim: *claim,
                    object: member.id,
                    byte_length: member.byte_length,
                    spool_offset: member.offset,
                });
            }
        }
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(MAX_SEMANTIC_SEGMENT_BYTES)
            .map_err(|_| "typed V2 single-segment buffer allocation failed".to_owned())?;
        buffer.resize(MAX_SEMANTIC_SEGMENT_BYTES, 0);
        Ok(Self {
            file: spool.clone_reader()?,
            segments,
            buffer,
        })
    }
}

impl TypedPlaneSegmentSourceV2 for TypedV2SpoolSegmentSource {
    type Error = String;

    fn segment<'source>(
        &'source mut self,
        index: usize,
        claim: &SemanticTypedPlaneSegmentClaimV2,
    ) -> Result<&'source [u8], Self::Error> {
        let admitted =
            self.segments.get(index).copied().ok_or_else(|| {
                "typed V2 verifier requested an unknown segment ordinal".to_owned()
            })?;
        if admitted.claim != *claim || admitted.byte_length != claim.byte_length() {
            return Err("typed V2 segment request differs from its admitted descriptor".to_owned());
        }
        let length = usize::try_from(admitted.byte_length)
            .map_err(|_| "typed V2 segment exceeds address space".to_owned())?;
        if length > MAX_SEMANTIC_SEGMENT_BYTES {
            return Err("typed V2 segment exceeds its fixed buffer".to_owned());
        }
        self.file
            .seek(SeekFrom::Start(admitted.spool_offset))
            .map_err(|error| format!("seek typed V2 history spool: {error}"))?;
        self.file
            .read_exact(&mut self.buffer[..length])
            .map_err(|error| format!("read typed V2 history spool segment: {error}"))?;
        Ok(&self.buffer[..length])
    }
}

fn verify_typed_v2_history_content(
    spool: &TypedV2HistorySpool,
    locator: &crate::ir_generation_store::TypedV2HistoryLocator,
    manifest: &SemanticTypedPlaneManifestV2,
    tier: SemanticTypedPlaneVerificationTierV2,
    jumbo_limits: JumboRopeLimits,
) -> Result<VerifiedTypedPlaneContentV2, String> {
    let mut segment_source = TypedV2SpoolSegmentSource::new(spool, locator, manifest)?;
    let mut jumbo_source = HistoryJumboSource::new(spool, &locator.jumbo)?;
    let verified = verify_typed_plane_content_v2_with_jumbo_segment_source(
        manifest,
        tier,
        jumbo_limits,
        &mut segment_source,
        &mut jumbo_source,
    )
    .map_err(|error| format!("verify typed V2 history content: {error}"))?;
    if jumbo_source.used.iter().any(|used| !used) {
        return Err("typed V2 history locator contains an unreferenced rope object".to_owned());
    }
    Ok(verified)
}

struct HistoryJumboSource<'a> {
    spool_file: File,
    spool_members: &'a [TypedV2SpoolMember],
    mappings: &'a [HistoryTypedV2JumboObject],
    used: Vec<bool>,
}

impl<'a> HistoryJumboSource<'a> {
    fn new(
        spool: &'a TypedV2HistorySpool,
        mappings: &'a [HistoryTypedV2JumboObject],
    ) -> Result<Self, String> {
        let mut used = Vec::new();
        used.try_reserve_exact(mappings.len())
            .map_err(|_| "typed V2 jumbo usage map allocation failed".to_owned())?;
        used.resize(mappings.len(), false);
        for mapping in mappings {
            let member = spool.member(mapping.object()).ok_or_else(|| {
                "typed V2 jumbo object is absent from the verified spool".to_owned()
            })?;
            if member.byte_length != mapping.byte_length()
                || member.schema != jumbo_schema_identity(mapping.kind())
            {
                return Err(
                    "typed V2 jumbo map differs from its verified spool envelope".to_owned(),
                );
            }
        }
        Ok(Self {
            spool_file: spool.clone_reader()?,
            spool_members: &spool.members,
            mappings,
            used,
        })
    }

    fn mapping_index(&self, id: JumboRopeObjectId, kind: JumboRopeObjectKind) -> Option<usize> {
        jumbo_mapping_index(self.mappings, id, kind)
    }

    fn read_payload(
        &mut self,
        mapping: HistoryTypedV2JumboObject,
        expected_kind: JumboRopeObjectKind,
        maximum_length: u64,
        output: &mut [u8],
    ) -> Result<Option<usize>, String> {
        if mapping.kind() != expected_kind {
            return Err("typed V2 history rope map uses the wrong object kind".to_owned());
        }
        if mapping.byte_length() == 0 || mapping.byte_length() > maximum_length {
            return Err("typed V2 history rope object exceeds its kind bound".to_owned());
        }
        let member = self
            .spool_members
            .binary_search_by(|member| member.id.as_bytes().cmp(mapping.object().as_bytes()))
            .ok()
            .and_then(|index| self.spool_members.get(index))
            .copied()
            .ok_or_else(|| "typed V2 history rope object disappeared from spool".to_owned())?;
        if member.byte_length != mapping.byte_length()
            || member.schema != jumbo_schema_identity(expected_kind)
        {
            return Err("typed V2 history rope spool envelope changed".to_owned());
        }
        let length = usize::try_from(member.byte_length)
            .map_err(|_| "typed V2 history rope length exceeds address space".to_owned())?;
        if length > output.len() {
            return Err("typed V2 history rope exceeds its bounded output".to_owned());
        }
        self.spool_file
            .seek(SeekFrom::Start(member.offset))
            .map_err(|error| format!("seek typed V2 history rope spool: {error}"))?;
        self.spool_file
            .read_exact(&mut output[..length])
            .map_err(|error| format!("read typed V2 history rope spool: {error}"))?;
        Ok(Some(length))
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
        let Some(length) = self.read_payload(
            mapping,
            JumboRopeObjectKind::Leaf,
            JUMBO_ROPE_MAX_LEAF_BYTES as u64,
            output,
        )?
        else {
            return Ok(None);
        };
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
        let mut output = [0_u8; ROPE_NODE_WIRE_BYTES];
        let Some(length) = self.read_payload(
            mapping,
            JumboRopeObjectKind::Interior,
            MAX_JUMBO_INTERIOR_BYTES,
            &mut output,
        )?
        else {
            return Ok(None);
        };
        if mapping.byte_length() != MAX_JUMBO_INTERIOR_BYTES || length != ROPE_NODE_WIRE_BYTES {
            return Err("typed V2 history rope interior has the wrong fixed length".to_owned());
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
        CanonicalPlaneSegmentBoundaryPolicy, ImageProvenance, JumboRopeLimits, JumboRopeNode,
        JumboRopeObjectId, JumboRopeObjectKind, JumboRopeObjectSink, JumboRopeWriteReceipt,
        JumboValueContext, JumboValueEncoding, JumboValueFamily, LanguageProfile, RustEdition,
        SemanticBuildIdentity, SemanticImageAuthority, SemanticImageFacts, SemanticInputClaimV2,
        SemanticIrPlane, SemanticPlaneKind, SemanticPlaneSegment,
        SemanticTypedPlaneFamilyDescriptorV2, SemanticTypedPlaneManifestV2,
        SemanticTypedPlaneSegmentClaimV2, UntrustedSemanticContentRootV2,
        UntrustedSemanticGenerationRootV2, UntrustedSemanticSegmentId, write_jumbo_value,
    };
    use backend_semantic::vocabulary::Stage;
    use backend_store::{
        ArtifactClosureClaim, ArtifactObjectClaim, StreamingClosureBudget, TypedObject,
    };
    use backend_version::{Coverage, ObjectKey, Schema, ScopeRoot};
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

    struct PositiveJumboLeafPayload;

    impl Schema for PositiveJumboLeafPayload {
        const DOMAIN: u8 = 0x52;
        const TYPE: u16 = 0xfff9;
        const VERSION: u8 = 1;
        type Value = [u8];

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    struct PositiveJumboInteriorPayload;

    impl Schema for PositiveJumboInteriorPayload {
        const DOMAIN: u8 = 0x52;
        const TYPE: u16 = 0xfffa;
        const VERSION: u8 = 1;
        type Value = [u8];

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    struct PositiveJumboObject {
        id: JumboRopeObjectId,
        kind: JumboRopeObjectKind,
        bytes: Vec<u8>,
    }

    #[derive(Default)]
    struct PositiveJumboObjects(Vec<PositiveJumboObject>);

    impl JumboRopeObjectSink for PositiveJumboObjects {
        type Error = Infallible;

        fn write_leaf(
            &mut self,
            leaf: backend_semantic::ir::JumboRopeLeafRef<'_>,
        ) -> Result<(), Self::Error> {
            self.0.push(PositiveJumboObject {
                id: leaf.id(),
                kind: JumboRopeObjectKind::Leaf,
                bytes: leaf.bytes().to_vec(),
            });
            Ok(())
        }

        fn write_interior(&mut self, node: &JumboRopeNode) -> Result<(), Self::Error> {
            self.0.push(PositiveJumboObject {
                id: node.id(),
                kind: JumboRopeObjectKind::Interior,
                bytes: node.encode_wire().to_vec(),
            });
            Ok(())
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
        let segments = if one_segment {
            vec![
                SemanticTypedPlaneSegmentClaimV2::from_untrusted_claims(
                    [1; 32],
                    [1; 32],
                    1,
                    1,
                    UntrustedSemanticSegmentId::from_raw([2; 32]),
                )
                .expect("one canonical segment claim"),
            ]
        } else {
            Vec::new()
        };
        typed_manifest_with_core_segments(segments, u64::from(one_segment))
    }

    fn typed_manifest_with_core_segments(
        core_segments: Vec<SemanticTypedPlaneSegmentClaimV2>,
        core_rows: u64,
    ) -> SemanticTypedPlaneManifestV2 {
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
        let boundary_policy =
            backend_semantic::ir::CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(
                MAX_SEMANTIC_SEGMENT_BYTES as u32,
                MAX_SEMANTIC_SEGMENT_BYTES as u32,
                MAX_SEMANTIC_SEGMENT_BYTES as u32,
            )
            .expect("terminal-only family boundary policy is valid");
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
            if family == SemanticIrPlane::Core {
                SemanticTypedPlaneFamilyDescriptorV2::from_untrusted_claims(
                    family,
                    core_rows,
                    boundary_policy,
                    core_segments.clone(),
                )
                .expect("core family descriptors")
            } else {
                SemanticTypedPlaneFamilyDescriptorV2::from_untrusted_claims(
                    family,
                    0,
                    boundary_policy,
                    Vec::new(),
                )
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

    type PositiveV2Row = ([u8; 32], u8, Vec<u8>);

    struct PositiveV2Segment {
        claim: SemanticTypedPlaneSegmentClaimV2,
        mapping: HistoryTypedV2SegmentObject,
        object: TypedObject,
    }

    pub(crate) struct PositiveV2HistoryFixture {
        pub(crate) manifest: SemanticTypedPlaneManifestV2,
        pub(crate) locator: crate::ir_generation_store::TypedV2HistoryLocator,
        pub(crate) objects: Vec<TypedObject>,
        pub(crate) expected_content_root: [u8; 32],
        pub(crate) expected_generation_root: [u8; 32],
    }

    fn positive_v2_profile() -> LanguageProfile {
        LanguageProfile::Rust(RustEdition::Rust2024)
    }

    fn positive_v2_build() -> SemanticBuildIdentity {
        SemanticBuildIdentity::new(
            [1; 32],
            [2; 32],
            positive_v2_profile(),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
            [6; 32],
        )
    }

    fn positive_v2_input_claim() -> SemanticInputClaimV2 {
        SemanticInputClaimV2::from_untrusted_claims(
            [7; 32],
            ScopeRoot::from_bytes([8; 32]),
            Coverage::Complete,
        )
    }

    fn positive_v2_identity() -> [u8; 32] {
        let mut identity = [0_u8; 32];
        identity[..16].fill(0x13);
        identity[16..].fill(0x41);
        identity
    }

    fn positive_v2_append_cell(bytes: &[u8], output: &mut Vec<u8>) {
        output.extend_from_slice(
            &u32::try_from(bytes.len())
                .expect("fixture cell length fits u32")
                .to_be_bytes(),
        );
        output.extend_from_slice(bytes);
    }

    fn positive_v2_types_rows(identity: [u8; 32]) -> Vec<PositiveV2Row> {
        const TYPES_KEY_DOMAIN: &[u8] = b"backend.semantic.ir.types-row-key.v1\0";
        let mut rows = Vec::new();
        let mut entity_payload = Vec::new();
        entity_payload.extend_from_slice(&identity);
        entity_payload.push(0); // no semantic type
        entity_payload.extend_from_slice(&[0; 32]);
        rows.push((identity, 1, entity_payload)); // entity-root row

        let type_parameters = [0_u8; 4];
        let mut key_hasher = blake3::Hasher::new();
        key_hasher.update(TYPES_KEY_DOMAIN);
        key_hasher.update(&[2, 8]); // Types plane, TypeParameters tag
        key_hasher.update(&type_parameters);
        rows.push((
            *key_hasher.finalize().as_bytes(),
            8,
            type_parameters.to_vec(),
        ));
        rows
    }

    fn positive_v2_rows(jumbo: &mut PositiveJumboObjects) -> [Vec<PositiveV2Row>; 7] {
        const CORE_DECLARATION_TAG: u8 = 1;
        const DOCUMENTATION_JUMBO_TAG: u8 = 3;
        const SOURCE_DECLARATION_TAG: u8 = 1;
        let identity = positive_v2_identity();
        let mut core_payload = Vec::new();
        core_payload.extend_from_slice(&identity);
        positive_v2_append_cell(b"fixture-name", &mut core_payload);
        core_payload.extend_from_slice(&0_u16.to_be_bytes()); // Function
        core_payload.push(0); // visibility
        core_payload.push(0); // no local parent
        core_payload.push(1); // authority root
        core_payload.extend_from_slice(&[
            0, 0, 0, 0, 1, // documentation captured
            0, 0, 0, // no language-extension owner
        ]);
        core_payload.extend_from_slice(&0_u32.to_be_bytes()); // members
        core_payload.extend_from_slice(&0_u32.to_be_bytes()); // attributes

        let mut docs_wire = Vec::new();
        docs_wire.extend_from_slice(&1_u32.to_be_bytes()); // one fragment
        docs_wire.push(0); // text fragment
        let text = vec![b'd'; 300 * 1024];
        docs_wire.extend_from_slice(
            &u32::try_from(text.len())
                .expect("fixture jumbo documentation length fits u32")
                .to_be_bytes(),
        );
        docs_wire.extend_from_slice(&text);
        let context = JumboValueContext::new(
            identity,
            JumboValueFamily::Documentation,
            0,
            JumboValueEncoding::Bytes,
        );
        let receipt = write_jumbo_value(context, &docs_wire, JumboRopeLimits::default(), jumbo)
            .expect("create nonempty documentation rope fixture");
        let mut documentation_payload = identity.to_vec();
        documentation_payload.push(1); // documentation captured
        documentation_payload.extend_from_slice(&receipt.verified().descriptor().encode_wire());

        let mut source_payload = identity.to_vec();
        source_payload.push(0); // source unavailable

        let mut rows: [Vec<PositiveV2Row>; 7] = core::array::from_fn(|_| Vec::new());
        rows[0].push((identity, CORE_DECLARATION_TAG, core_payload));
        rows[1] = positive_v2_types_rows(identity);
        rows[4].push((identity, DOCUMENTATION_JUMBO_TAG, documentation_payload));
        rows[5].push((identity, SOURCE_DECLARATION_TAG, source_payload));
        rows
    }

    fn positive_v2_family_kinds(profile: LanguageProfile) -> [SemanticIrPlane; 7] {
        [
            SemanticIrPlane::Core,
            SemanticIrPlane::Types,
            SemanticIrPlane::Relations,
            SemanticIrPlane::Occurrences,
            SemanticIrPlane::Documentation,
            SemanticIrPlane::SourceProvenance,
            SemanticIrPlane::LanguageExtensions(profile),
        ]
    }

    fn positive_v2_plane_code(family: SemanticIrPlane) -> u8 {
        match family {
            SemanticIrPlane::Core => 1,
            SemanticIrPlane::Types => 2,
            SemanticIrPlane::Relations => 3,
            SemanticIrPlane::Occurrences => 4,
            SemanticIrPlane::Documentation => 5,
            SemanticIrPlane::SourceProvenance => 6,
            SemanticIrPlane::LanguageExtensions(_) => 7,
        }
    }

    fn positive_v2_encode_segment(
        family: SemanticIrPlane,
        mut rows: Vec<PositiveV2Row>,
    ) -> PositiveV2Segment {
        rows.sort_unstable_by_key(|(key, _, _)| *key);
        let first_key = rows
            .first()
            .map(|(key, _, _)| *key)
            .expect("nonempty fixture family");
        let last_key = rows
            .last()
            .map(|(key, _, _)| *key)
            .expect("nonempty fixture family");
        let row_count = u32::try_from(rows.len()).expect("fixture row count fits u32");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"SPIR");
        bytes.extend_from_slice(&2_u16.to_be_bytes()); // c004 segment wire revision
        bytes.push(positive_v2_plane_code(family));
        bytes.extend_from_slice(&row_count.to_be_bytes());
        for (key, tag, payload) in rows {
            bytes.extend_from_slice(&key);
            bytes.push(tag);
            bytes.extend_from_slice(
                &u32::try_from(payload.len())
                    .expect("fixture row payload length fits u32")
                    .to_be_bytes(),
            );
            bytes.extend_from_slice(&payload);
        }
        let descriptor = SemanticPlaneSegment::from_payload(
            SemanticPlaneKind::Ir(family),
            first_key,
            last_key,
            row_count,
            &bytes,
        )
        .expect("canonical fixture segment descriptor");
        let segment_id = descriptor.admitted_id().expect("descriptor admits its ID");
        let claim = SemanticTypedPlaneSegmentClaimV2::from_untrusted_claims(
            first_key,
            last_key,
            row_count,
            u64::try_from(bytes.len()).expect("fixture segment length fits u64"),
            UntrustedSemanticSegmentId::from_raw(*segment_id.as_bytes()),
        )
        .expect("typed V2 segment claim is canonical");
        let object = TypedObject::from_value(
            &ObjectKey::<super::super::SemanticSegmentPayload>::from_value(bytes.as_slice()),
            bytes.as_slice(),
        );
        let mapping = HistoryTypedV2SegmentObject::new(
            UntrustedSemanticSegmentId::from_raw(*segment_id.as_bytes()),
            UntrustedObjectId::from_bytes(*object.id().as_bytes()),
            u64::try_from(bytes.len()).expect("fixture segment length fits u64"),
        );
        PositiveV2Segment {
            claim,
            mapping,
            object,
        }
    }

    fn positive_v2_expected_roots(
        build: SemanticBuildIdentity,
        input: SemanticInputClaimV2,
        rows: &[Vec<PositiveV2Row>; 7],
    ) -> Result<([u8; 32], [u8; 32]), backend_semantic::ir::row_index::StableRowIndexError> {
        use backend_semantic::ir::row_index::{
            RowFamily, RowPayload, StableRowIndex, StableRowKey,
        };

        const FAMILY_ROW_ROOT_DOMAIN: &[u8] = b"backend.semantic.ir.family-row-index-root.v2\0";
        let mut family_roots = [[0_u8; 32]; 7];
        let mut row_counts = [0_u64; 7];
        for index in 0..7 {
            let family = *RowFamily::ALL.get(index).expect("seven row family slots");
            let mut index_rows = rows
                .get(index)
                .expect("seven fixture families")
                .iter()
                .map(|(key, tag, payload)| {
                    Ok::<_, backend_semantic::ir::row_index::StableRowIndexError>((
                        StableRowKey::new(family, *key),
                        RowPayload::from_tagged_bytes(*tag, payload)?,
                    ))
                })
                .collect::<Result<Vec<_>, _>>()?;
            index_rows.sort_unstable_by_key(|(key, _)| *key);
            let index_root = StableRowIndex::from_sorted_rows(&index_rows)?
                .root()
                .as_bytes();
            let row_count = u64::try_from(index_rows.len()).expect("fixture row count fits u64");
            row_counts[index] = row_count;
            let mut hasher = blake3::Hasher::new();
            hasher.update(FAMILY_ROW_ROOT_DOMAIN);
            hasher.update(&[2, family.code()]);
            if index == 6 {
                hasher.update(&<[u8; 2]>::from(build.profile()));
            }
            hasher.update(&row_count.to_be_bytes());
            hasher.update(&index_root);
            family_roots[index] = *hasher.finalize().as_bytes();
        }

        let mut content_hasher = blake3::Hasher::new_derive_key("backend.semantic.ir.content.v2");
        content_hasher.update(&[2]);
        content_hasher.update(&[0]); // Shared semantic image authority
        content_hasher.update(&7_u16.to_be_bytes());
        for index in 0..7 {
            content_hasher.update(&[u8::try_from(index).expect("family tag fits u8")]);
            if index == 6 {
                content_hasher.update(&<[u8; 2]>::from(build.profile()));
            }
            content_hasher.update(family_roots.get(index).expect("seven family roots"));
            content_hasher.update(
                &row_counts
                    .get(index)
                    .expect("seven row counts")
                    .to_be_bytes(),
            );
        }
        let content_root = *content_hasher.finalize().as_bytes();

        let mut generation_hasher =
            blake3::Hasher::new_derive_key("backend.semantic.ir.generation.v2");
        generation_hasher.update(&[2]);
        generation_hasher.update(&content_root);
        generation_hasher.update(build.package());
        generation_hasher.update(build.target());
        generation_hasher.update(&<[u8; 2]>::from(build.profile()));
        generation_hasher.update(&[u8::from(build.stage())]);
        generation_hasher.update(build.recipe());
        generation_hasher.update(build.toolchain());
        generation_hasher.update(build.environment());
        generation_hasher.update(build.target_platform());
        generation_hasher.update(&input.as_claimed_witness().generation_root_commitment_v2());
        Ok((content_root, *generation_hasher.finalize().as_bytes()))
    }

    fn positive_v2_fixture() -> (
        SemanticTypedPlaneManifestV2,
        crate::ir_generation_store::TypedV2HistoryLocator,
        Vec<TypedObject>,
        [u8; 32],
        [u8; 32],
    ) {
        let mut jumbo = PositiveJumboObjects::default();
        let rows = positive_v2_rows(&mut jumbo);
        assert_eq!(
            jumbo.0.len(),
            3,
            "large fixture value uses two leaves and an interior"
        );
        let profile = positive_v2_profile();
        let family_kinds = positive_v2_family_kinds(profile);
        let mut segment_claims: Vec<Vec<SemanticTypedPlaneSegmentClaimV2>> = Vec::new();
        let mut segment_mappings = Vec::new();
        let mut objects = Vec::new();
        for (index, family) in family_kinds.iter().copied().enumerate() {
            let family_rows = rows.get(index).expect("seven fixture families").clone();
            if family_rows.is_empty() {
                segment_claims.push(Vec::new());
                continue;
            }
            let segment = positive_v2_encode_segment(family, family_rows);
            segment_claims.push(vec![segment.claim]);
            segment_mappings.push(segment.mapping);
            objects.push(segment.object);
        }
        segment_mappings.sort_unstable_by(|left, right| {
            left.segment().as_bytes().cmp(right.segment().as_bytes())
        });

        let mut jumbo_mappings = Vec::new();
        for object in jumbo.0 {
            let typed = match object.kind {
                JumboRopeObjectKind::Leaf => TypedObject::from_value(
                    &ObjectKey::<PositiveJumboLeafPayload>::from_value(object.bytes.as_slice()),
                    object.bytes.as_slice(),
                ),
                JumboRopeObjectKind::Interior => TypedObject::from_value(
                    &ObjectKey::<PositiveJumboInteriorPayload>::from_value(object.bytes.as_slice()),
                    object.bytes.as_slice(),
                ),
            };
            jumbo_mappings.push(HistoryTypedV2JumboObject::new(
                object.id,
                object.kind,
                UntrustedObjectId::from_bytes(*typed.id().as_bytes()),
                u64::try_from(object.bytes.len()).expect("fixture jumbo length fits u64"),
            ));
            objects.push(typed);
        }
        jumbo_mappings.sort_unstable_by_key(|mapping| jumbo_map_order(*mapping));

        let build = positive_v2_build();
        let input = positive_v2_input_claim();
        let (content_root, generation_root) = positive_v2_expected_roots(build, input, &rows)
            .expect("independent stable-row-index root calculation");
        let boundary_policy = CanonicalPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(
            MAX_SEMANTIC_SEGMENT_BYTES as u32,
            MAX_SEMANTIC_SEGMENT_BYTES as u32,
            MAX_SEMANTIC_SEGMENT_BYTES as u32,
        )
        .expect("terminal-only fixture boundary policy");
        let mut families = Vec::new();
        for (index, family) in family_kinds.iter().copied().enumerate() {
            let count =
                u64::try_from(rows[index].len()).expect("fixture family row count fits u64");
            families.push(
                SemanticTypedPlaneFamilyDescriptorV2::from_untrusted_claims(
                    family,
                    count,
                    boundary_policy,
                    core::mem::take(segment_claims.get_mut(index).expect("seven claim slots")),
                )
                .expect("c007 family claim is canonical"),
            );
        }
        let families: [SemanticTypedPlaneFamilyDescriptorV2; 7] =
            families.try_into().expect("exact seven fixture families");
        let manifest = SemanticTypedPlaneManifestV2::from_untrusted_claims(
            build,
            SemanticImageFacts {
                authority: SemanticImageAuthority::Shared,
                provenance: ImageProvenance::Unavailable,
            },
            input,
            UntrustedSemanticContentRootV2::from_wire_claim(content_root),
            UntrustedSemanticGenerationRootV2::from_wire_claim(generation_root),
            families,
        )
        .expect("nonempty c007 claims are structurally canonical");
        let locator = crate::ir_generation_store::TypedV2HistoryLocator {
            manifest: manifest
                .canonical_bytes()
                .expect("encode positive c007 manifest"),
            segments: segment_mappings,
            jumbo: jumbo_mappings,
        };
        locator
            .validate()
            .expect("positive V2 locator is canonical");
        (manifest, locator, objects, content_root, generation_root)
    }

    pub(crate) fn positive_v2_history_fixture_for_test() -> PositiveV2HistoryFixture {
        let (manifest, locator, objects, expected_content_root, expected_generation_root) =
            positive_v2_fixture();
        PositiveV2HistoryFixture {
            manifest,
            locator,
            objects,
            expected_content_root,
            expected_generation_root,
        }
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

    #[test]
    fn cold_v2_reopens_nonempty_seven_family_segments_and_jumbo_rope() {
        let directory = TestDirectory::create();
        let (manifest, locator, objects, expected_content, expected_generation) =
            positive_v2_fixture();
        let total_payload_bytes = objects
            .iter()
            .try_fold(0_u64, |total, object| {
                total.checked_add(u64::try_from(object.bytes().len()).ok()?)
            })
            .expect("fixture closure payload size fits u64");
        let maximum_object_bytes = objects
            .iter()
            .map(|object| object.bytes().len())
            .max()
            .expect("fixture closure has segments and jumbo objects");
        let chunk_bytes = IO_BUFFER_BYTES;
        let chunk_calls = objects
            .iter()
            .try_fold(0_usize, |calls, object| {
                let object_calls = object
                    .bytes()
                    .len()
                    .checked_add(chunk_bytes - 1)?
                    .checked_div(chunk_bytes)?;
                calls.checked_add(object_calls)
            })
            .expect("fixture object chunk count fits usize");
        let object_count = objects.len();
        let closure_claim = {
            let store = FileStore::open(&directory.0, 1024 * 1024)
                .expect("create FileStore for positive cold fixture");
            let metadata_bytes = StreamingClosureBudget::metadata_input_bytes_for(object_count)
                .expect("fixture closure metadata size fits usize");
            let mut builder = store
                .begin_streaming_closure(StreamingClosureBudget::new(
                    object_count,
                    total_payload_bytes,
                    maximum_object_bytes,
                    chunk_bytes,
                    chunk_calls,
                    metadata_bytes,
                ))
                .expect("begin exact positive fixture closure");
            for object in &objects {
                let claim = ArtifactObjectClaim::new(
                    object.schema(),
                    *object.key(),
                    *object.version(),
                    u64::try_from(object.bytes().len()).expect("fixture object length fits u64"),
                )
                .with_object_id(UntrustedObjectId::from_bytes(*object.id().as_bytes()));
                let mut stream = builder
                    .begin_object(claim)
                    .expect("begin exact fixture object");
                for chunk in object.bytes().chunks(chunk_bytes) {
                    stream.write(chunk).expect("write fixture object chunk");
                }
                assert_eq!(stream.finish().expect("admit fixture object"), object.id());
            }
            let receipt = builder.seal().expect("seal exact fixture closure");
            ArtifactClosureClaim::from_id(receipt.closure())
        };

        // Reopen through a fresh FileStore handle so the following proof uses
        // only durable manifests, object envelopes, and the cold spool path.
        let store = FileStore::open(&directory.0, 1024 * 1024)
            .expect("reopen FileStore after simulated process restart");
        let closure = store
            .open_closure_claim(closure_claim)
            .expect("open exact nonempty V2 closure after restart");
        assert_eq!(
            closure.object_count(),
            u64::try_from(object_count).expect("fixture count fits u64")
        );
        let spool = spool_typed_v2_history_closure(
            &store,
            closure_claim,
            &closure,
            &locator,
            &manifest,
            SemanticTypedPlaneVerificationTierV2::Standard,
        )
        .expect("verify and stage one exact nonempty closure pass");
        assert_eq!(spool.members.len(), object_count);
        assert_eq!(spool.bytes_written, total_payload_bytes);
        #[cfg(unix)]
        assert!(
            spool.path.is_none(),
            "Unix spool has no crash-leak pathname"
        );

        let verified = verify_typed_v2_history_content(
            &spool,
            &locator,
            &manifest,
            SemanticTypedPlaneVerificationTierV2::Standard,
            JumboRopeLimits::default(),
        )
        .expect("verify seven-family rows and complete documentation rope");
        assert_eq!(verified.content_root().as_bytes(), &expected_content);
        assert_eq!(verified.generation_root().as_bytes(), &expected_generation);
        let expected_rows = [1_u64, 2, 0, 0, 1, 1, 0];
        for (family, expected) in verified.families().iter().zip(expected_rows) {
            assert_eq!(family.row_count(), expected);
        }
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
        let locator = crate::ir_generation_store::TypedV2HistoryLocator {
            manifest: manifest.canonical_bytes().expect("encode manifest"),
            segments: vec![HistoryTypedV2SegmentObject::new(
                UntrustedSemanticSegmentId::from_raw([2; 32]),
                UntrustedObjectId::from_bytes([3; 32]),
                1,
            )],
            jumbo: Vec::new(),
        };
        locator.validate().expect("canonical locator map");
        let error = spool_typed_v2_history_closure(
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
    fn large_tier_segment_source_reuses_one_fixed_buffer_for_512_mib_shape() {
        const SEGMENT_COUNT: usize = 512;
        const SEGMENT_BYTES: usize = MAX_SEMANTIC_SEGMENT_BYTES;
        let tier_bytes = u64::try_from(SEGMENT_COUNT)
            .expect("fixture segment count fits u64")
            .checked_mul(u64::try_from(SEGMENT_BYTES).expect("segment size fits u64"))
            .expect("fixture tier byte count fits u64");
        assert_eq!(tier_bytes, MAX_TYPED_V2_LARGE_BYTES);

        let directory = TestDirectory::create();
        let spool_path = directory.0.join("sparse-512-mib-shape.spool");
        let spool_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&spool_path)
            .expect("create sparse tier-shape spool");
        spool_file
            .set_len(tier_bytes)
            .expect("reserve sparse 512 MiB shape");
        let segment_schema = crate::ProducedSemanticObjectKind::Segment.schema_identity();
        let representative_payload = vec![0_u8; SEGMENT_BYTES];
        let representative_object = TypedObject::from_value(
            &ObjectKey::<super::super::SemanticSegmentPayload>::from_value(
                representative_payload.as_slice(),
            ),
            representative_payload.as_slice(),
        );
        let object_id = representative_object.id();
        drop(representative_object);
        drop(representative_payload);
        let object_claim = UntrustedObjectId::from_bytes(*object_id.as_bytes());
        let mut claims = Vec::new();
        let mut mappings = Vec::new();
        let mut members = Vec::new();
        claims
            .try_reserve_exact(SEGMENT_COUNT)
            .expect("reserve tier-shape claims");
        mappings
            .try_reserve_exact(SEGMENT_COUNT)
            .expect("reserve tier-shape locator mappings");
        members
            .try_reserve_exact(1)
            .expect("reserve tier-shape spool index");
        for index in 0..SEGMENT_COUNT {
            let ordinal = u32::try_from(index + 1).expect("fixture ordinal fits u32");
            let mut first_key = [0_u8; 32];
            first_key[28..].copy_from_slice(&ordinal.to_be_bytes());
            let segment_id = UntrustedSemanticSegmentId::from_raw(first_key);
            let claim = SemanticTypedPlaneSegmentClaimV2::from_untrusted_claims(
                first_key,
                first_key,
                1,
                u64::try_from(SEGMENT_BYTES).expect("segment size fits u64"),
                segment_id,
            )
            .expect("tier-shape segment claim");
            claims.push(claim);
            mappings.push(HistoryTypedV2SegmentObject::new(
                segment_id,
                object_claim,
                u64::try_from(SEGMENT_BYTES).expect("segment size fits u64"),
            ));
        }
        members.push(TypedV2SpoolMember {
            id: object_id,
            byte_length: u64::try_from(SEGMENT_BYTES).expect("segment size fits u64"),
            schema: segment_schema,
            offset: 0,
        });
        let manifest = typed_manifest_with_core_segments(
            claims.clone(),
            u64::try_from(SEGMENT_COUNT).expect("fixture segment count fits u64"),
        );
        let locator = super::super::ir_generation_store::TypedV2HistoryLocator {
            manifest: manifest
                .canonical_bytes()
                .expect("encode tier-shape manifest"),
            segments: mappings,
            jumbo: Vec::new(),
        };
        let spool = TypedV2HistorySpool {
            path: Some(spool_path),
            file: spool_file,
            members,
            bytes_written: tier_bytes,
        };
        let mut source = TypedV2SpoolSegmentSource::new(&spool, &locator, &manifest)
            .expect("construct bounded source for the full segment tier");
        assert_eq!(source.segments.len(), SEGMENT_COUNT);
        assert_eq!(source.buffer.len(), SEGMENT_BYTES);

        let first_ptr = {
            let first = source
                .segment(0, &claims[0])
                .expect("lend first tier-shape segment");
            assert_eq!(first.len(), SEGMENT_BYTES);
            assert!(first.iter().all(|byte| *byte == 0));
            first.as_ptr()
        };
        let last_ptr = {
            let last = source
                .segment(SEGMENT_COUNT - 1, &claims[SEGMENT_COUNT - 1])
                .expect("lend last tier-shape segment");
            assert_eq!(last.len(), SEGMENT_BYTES);
            assert!(last.iter().all(|byte| *byte == 0));
            last.as_ptr()
        };
        assert_eq!(first_ptr, last_ptr, "segment payload storage is reused");
    }

    #[test]
    fn duplicate_physical_segment_membership_deduplicates_closure_but_mints_no_proof() {
        let directory = TestDirectory::create();
        let store = FileStore::open(&directory.0, 1024 * 1024).expect("open test FileStore");
        let payload = b"not a canonical c004 segment";
        let typed = TypedObject::from_value(
            &ObjectKey::<super::super::SemanticSegmentPayload>::from_value(payload),
            payload,
        );
        let object_claim = ArtifactObjectClaim::new(
            typed.schema(),
            *typed.key(),
            *typed.version(),
            u64::try_from(payload.len()).expect("bounded test payload length"),
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
            .expect("begin one-object segment closure");
        let mut stream = builder
            .begin_object(object_claim)
            .expect("begin typed segment object");
        stream.write(payload).expect("write typed segment object");
        let object = stream.finish().expect("admit typed segment object");
        let closure_id = builder.seal().expect("seal one-object segment closure");
        let closure_claim = ArtifactClosureClaim::from_id(closure_id.closure());
        let closure = store
            .open_closure_claim(closure_claim)
            .expect("open one-object segment closure");

        let byte_length = u64::try_from(payload.len()).expect("bounded test payload length");
        let claims = vec![
            SemanticTypedPlaneSegmentClaimV2::from_untrusted_claims(
                [1; 32],
                [1; 32],
                1,
                byte_length,
                UntrustedSemanticSegmentId::from_raw([2; 32]),
            )
            .expect("first logical segment claim"),
            SemanticTypedPlaneSegmentClaimV2::from_untrusted_claims(
                [3; 32],
                [3; 32],
                1,
                byte_length,
                UntrustedSemanticSegmentId::from_raw([4; 32]),
            )
            .expect("second logical segment claim"),
        ];
        let manifest = typed_manifest_with_core_segments(claims, 2);
        let locator = super::super::ir_generation_store::TypedV2HistoryLocator {
            manifest: manifest
                .canonical_bytes()
                .expect("encode duplicate-map manifest"),
            segments: vec![
                HistoryTypedV2SegmentObject::new(
                    UntrustedSemanticSegmentId::from_raw([2; 32]),
                    UntrustedObjectId::from_bytes(*object.as_bytes()),
                    byte_length,
                ),
                HistoryTypedV2SegmentObject::new(
                    UntrustedSemanticSegmentId::from_raw([4; 32]),
                    UntrustedObjectId::from_bytes(*object.as_bytes()),
                    byte_length,
                ),
            ],
            jumbo: Vec::new(),
        };
        locator
            .validate()
            .expect("canonical duplicate physical map");
        let spool = spool_typed_v2_history_closure(
            &store,
            closure_claim,
            &closure,
            &locator,
            &manifest,
            SemanticTypedPlaneVerificationTierV2::Standard,
        )
        .expect("one closure member covers duplicate physical locator IDs");
        assert_eq!(spool.members.len(), 1, "closure set is physical-ID unique");
        assert!(
            verify_typed_v2_history_content(
                &spool,
                &locator,
                &manifest,
                SemanticTypedPlaneVerificationTierV2::Standard,
                JumboRopeLimits::default(),
            )
            .is_err(),
            "one physical payload cannot satisfy two distinct semantic segment claims"
        );
    }

    #[test]
    fn cold_v2_rejects_extra_closure_members_and_wrong_jumbo_schema() {
        let directory = TestDirectory::create();
        let store = FileStore::open(&directory.0, 1024 * 1024).expect("open test FileStore");
        let empty = empty_closure(&store);
        let (extra_claim, _) = one_object_closure(&store, test_payload_schema(), b"extra");
        let empty_manifest = empty_manifest();
        let empty_locator = crate::ir_generation_store::TypedV2HistoryLocator {
            manifest: empty_manifest.canonical_bytes().expect("encode manifest"),
            segments: Vec::new(),
            jumbo: Vec::new(),
        };
        let extra = store
            .open_closure_claim(extra_claim)
            .expect("reopen extra-member closure");
        let error = spool_typed_v2_history_closure(
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
        let wrong_kind_locator = crate::ir_generation_store::TypedV2HistoryLocator {
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
        let error = spool_typed_v2_history_closure(
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

#[cfg(test)]
pub(crate) use tests::{PositiveV2HistoryFixture, positive_v2_history_fixture_for_test};
