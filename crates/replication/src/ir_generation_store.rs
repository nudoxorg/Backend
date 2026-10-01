//! Durable selected semantic-image generations for local IR consumers.
//!
//! Generation records are immutable canonical catalog/manifest pairs. The
//! small per-target head is the only mutable pointer and carries the authority
//! stamp separately from the content-derived generation identity.

#[cfg(test)]
use std::cell::Cell;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use backend_semantic::ir::{
    GenerationId, SemanticDeltaCursor, SemanticImageIdentity, SemanticManifestError,
    SemanticManifestRoot, SemanticPlaneCatalog, SemanticPlaneImageKey, SemanticPlaneKind,
    SemanticPlaneManifest, SemanticPlaneRoot,
};

use crate::{SelectedGenerationSource, SelectedGenerationStamp, SemanticTargetKey};

const MAGIC: [u8; 4] = *b"SIRG";
const VERSION: u8 = 1;
const RECORD_TAG: u8 = 1;
const HEAD_TAG: u8 = 2;
const RECORDS_EPOCH_TAG: u8 = 3;
const CHECKSUM_BYTES: usize = 32;
const MAX_TARGET_FIELD_BYTES: usize = 4 * 1024;
const MAX_CATALOG_BYTES: usize = 8 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
const MAX_GENERATION_RECORD_BYTES: usize = 72 * 1024 * 1024;
const MAX_HEAD_BYTES: usize = 320;
const MAX_GENERATION_SCAN_MEMBERS: usize = 4_096;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HistoryTestFault {
    AfterGenerationRecord,
    AfterHistoryCommit,
    AfterTypedV2Locator,
    AfterHistoryIndex,
    AfterPayloadRoot,
    AfterRefsCatalog,
    AfterHead,
    AfterHistoryMapUnlink,
    AfterHistoryIndexCompactRename,
}

#[cfg(test)]
thread_local! {
    static HISTORY_TEST_FAULT: Cell<Option<HistoryTestFault>> = const { Cell::new(None) };
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct HistoryReplayLoadCounts {
    pub(super) commit_decodes: usize,
    pub(super) generation_decodes: usize,
    pub(super) locator_decodes: usize,
}

#[cfg(test)]
thread_local! {
    static HISTORY_REPLAY_LOAD_COUNTS: Cell<HistoryReplayLoadCounts> =
        const {
            Cell::new(HistoryReplayLoadCounts {
                commit_decodes: 0,
                generation_decodes: 0,
                locator_decodes: 0,
            })
        };
}

#[cfg(test)]
pub(super) fn reset_history_replay_load_counts() {
    HISTORY_REPLAY_LOAD_COUNTS.with(|counts| counts.set(HistoryReplayLoadCounts::default()));
}

#[cfg(test)]
pub(super) fn history_replay_load_counts() -> HistoryReplayLoadCounts {
    HISTORY_REPLAY_LOAD_COUNTS.with(Cell::get)
}

#[cfg(test)]
fn count_history_commit_decode() {
    HISTORY_REPLAY_LOAD_COUNTS.with(|counts| {
        let mut current = counts.get();
        current.commit_decodes = current.commit_decodes.saturating_add(1);
        counts.set(current);
    });
}

#[cfg(test)]
fn count_history_generation_decode() {
    HISTORY_REPLAY_LOAD_COUNTS.with(|counts| {
        let mut current = counts.get();
        current.generation_decodes = current.generation_decodes.saturating_add(1);
        counts.set(current);
    });
}

#[cfg(test)]
fn count_history_locator_decode() {
    HISTORY_REPLAY_LOAD_COUNTS.with(|counts| {
        let mut current = counts.get();
        current.locator_decodes = current.locator_decodes.saturating_add(1);
        counts.set(current);
    });
}

#[cfg(test)]
pub(super) fn arm_history_test_fault(point: HistoryTestFault) {
    HISTORY_TEST_FAULT.with(|fault| fault.set(Some(point)));
}

#[cfg(test)]
pub(super) fn trip_history_test_fault(point: HistoryTestFault) -> Result<(), String> {
    HISTORY_TEST_FAULT.with(|fault| {
        if fault.get() == Some(point) {
            fault.set(None);
            Err(format!(
                "injected semantic history interruption at {point:?}"
            ))
        } else {
            Ok(())
        }
    })
}

mod history;
pub(crate) use history::TypedV2HistoryLocator;
pub(crate) use history::TypedV2HistoryPublicationAdmission;
pub(crate) use history::TypedV2HistoryPublicationSnapshot;
pub(crate) use history::TypedV3HistoryLocator;
pub use history::{
    AdmittedHistoryCommit, BorrowedTypedLineageEdgeSetV1, HistoryAdmissionReceipt, HistoryCommitId,
    HistoryGcProgress, HistoryGcStats, HistoryGenerationRoot, HistoryMaterialization,
    HistoryProposalError, HistoryRefAncestryProof, HistoryRefKind, HistoryRefName,
    HistoryRefUpdateReceipt, HistoryReplay, HistoryReplayCursor, HistoryReplayEntry,
    HistorySegmentDeltas, HistoryTypedV2JumboObject, HistoryTypedV2LocatorId,
    HistoryTypedV2RootClaim, HistoryTypedV2SegmentObject, HistoryTypedV3LocatorId,
    HistoryTypedV3RootClaim, LineageAttestationId, LineageAttestationVerifierV1,
    LineageCandidateGroupIdV1, LineageConfirmationStatementV1, LineageEdgeIterV1,
    LineageEdgeSetErrorV1, LineageEdgeV1, LineageEdgeViewV1, LineageHistoryEvidenceV1,
    LineageKindV1, LineageSourceV1, LineageStatusV1, LineageStatusViewV1,
    MAX_HISTORY_REPLAY_COMMITS, MAX_LINEAGE_CANDIDATES_PER_GROUP_V1,
    MAX_TYPED_LINEAGE_EDGE_SET_V1_BYTES, MAX_TYPED_LINEAGE_EDGES_V1, OwnedTypedLineageEdgeSetV1,
    RejectLineageConfirmationsV1, SelectedHistoryRef, TypedV2HistoryReplay,
    UnprovenTypedLineageEdgeSetV1, UnpublishedHistoryProposal, UnresolvedLineageReasonV1,
    VerifiedLineageEdgeIterV1, VerifiedLineageEdgeViewV1, VerifiedLineageRootV2,
    VerifiedLineageStatusV1, VerifiedTypedLineageEdgeSetV1,
};
pub(super) use history::{AdmittedHistoryPayloadRoot, HistoryPayloadRoot};

/// Content identity of one admitted target/catalog/image-manifest tuple.
///
/// It deliberately excludes the selected authority revision. The authority
/// stamp belongs to the mutable head observation; identical semantic metadata
/// can therefore remain one immutable local generation across authority reads.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LocalSemanticGenerationId([u8; 32]);

impl LocalSemanticGenerationId {
    /// Returns the content-derived identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// A historical local CAS owner binding recovered from one validated local
/// generation record.
///
/// This names the old selection-scoped CAS mapping only. It is not evidence
/// that the old generation is currently selected. Consumers must freshly
/// select the target and verify candidate bytes against its exact descriptor
/// before exposing or adopting them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoricalSemanticPlaneBinding {
    generation: LocalSemanticGenerationId,
    stamp: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
    kind: SemanticPlaneKind,
    manifest_root: SemanticManifestRoot,
    plane_root: SemanticPlaneRoot,
}

impl HistoricalSemanticPlaneBinding {
    /// Returns the checksummed local generation record that admitted this
    /// historical CAS owner.
    #[must_use]
    pub const fn generation(self) -> LocalSemanticGenerationId {
        self.generation
    }

    /// Returns the historical authority stamp used only to address its local
    /// selection-scoped CAS mapping.
    #[must_use]
    pub(crate) const fn stamp(self) -> SelectedGenerationStamp {
        self.stamp
    }

    /// Returns the image identity stored in the validated local record.
    #[must_use]
    pub(crate) const fn image(self) -> SemanticPlaneImageKey {
        self.image
    }

    /// Returns the semantic plane named by this historical owner.
    #[must_use]
    pub(crate) const fn kind(self) -> SemanticPlaneKind {
        self.kind
    }

    /// Returns the canonical manifest root stored in the local record.
    #[must_use]
    pub(crate) const fn manifest_root(self) -> SemanticManifestRoot {
        self.manifest_root
    }

    /// Returns the canonical plane root stored in the local record.
    #[must_use]
    pub(crate) const fn plane_root(self) -> SemanticPlaneRoot {
        self.plane_root
    }

    pub(crate) fn matches_manifest(
        self,
        manifest: &SemanticPlaneManifest,
        kind: SemanticPlaneKind,
    ) -> bool {
        self.kind == kind
            && self.manifest_root == manifest.root()
            && self.image.manifest_root() == manifest.root()
            && self.image.semantic_generation() == manifest.semantic_generation()
            && self.stamp.profile() == manifest.build().profile()
            && manifest
                .plane(kind)
                .is_some_and(|plane| plane.root() == self.plane_root)
    }
}

/// One cold-reopenable local selected image generation.
///
/// The catalog and manifest are decoded from the exact canonical bytes saved
/// before the local head moved. Coverage authority is not recreated by decode;
/// consumers must obtain fresh selection evidence for authority-sensitive use.
#[derive(Debug)]
pub struct LocalSemanticGeneration {
    identity: LocalSemanticGenerationId,
    previous_identity: Option<LocalSemanticGenerationId>,
    previous_image: Option<SemanticPlaneImageKey>,
    previous_image_identity: Option<SemanticImageIdentity>,
    target: SemanticTargetKey,
    selected_stamp: SelectedGenerationStamp,
    catalog: SemanticPlaneCatalog,
    image: SemanticPlaneImageKey,
    image_identity: SemanticImageIdentity,
    manifest: SemanticPlaneManifest,
}

impl LocalSemanticGeneration {
    /// Returns this immutable local generation identity.
    #[must_use]
    pub const fn identity(&self) -> LocalSemanticGenerationId {
        self.identity
    }

    /// Returns the preceding local generation identity retained by the head.
    #[must_use]
    pub const fn previous_identity(&self) -> Option<LocalSemanticGenerationId> {
        self.previous_identity
    }

    /// Exact canonical image key retained as this head's local predecessor,
    /// when both immutable records remain available.
    #[must_use]
    pub const fn previous_image(&self) -> Option<SemanticPlaneImageKey> {
        self.previous_image
    }

    /// Content identity of the preceding complete NXFI, if it remains in the
    /// immutable generation record.
    #[must_use]
    pub const fn previous_image_identity(&self) -> Option<SemanticImageIdentity> {
        self.previous_image_identity
    }

    /// Returns the target whose catalog was admitted.
    #[must_use]
    pub const fn target(&self) -> &SemanticTargetKey {
        &self.target
    }

    /// Returns the authority stamp observed when this head was published.
    #[must_use]
    pub const fn selected_stamp(&self) -> SelectedGenerationStamp {
        self.selected_stamp
    }

    /// Returns the admitted canonical catalog.
    #[must_use]
    pub const fn catalog(&self) -> &SemanticPlaneCatalog {
        &self.catalog
    }

    /// Returns the exact catalog image whose manifest is retained.
    #[must_use]
    pub const fn image(&self) -> SemanticPlaneImageKey {
        self.image
    }

    /// Returns the typed content identity of the persisted complete NXFI image.
    #[must_use]
    pub const fn image_identity(&self) -> SemanticImageIdentity {
        self.image_identity
    }

    /// Returns the canonical semantic IR generation named by the image.
    #[must_use]
    pub const fn semantic_generation(&self) -> GenerationId {
        self.image.semantic_generation()
    }

    /// Returns the canonical selected image manifest.
    #[must_use]
    pub const fn manifest(&self) -> &SemanticPlaneManifest {
        &self.manifest
    }

    /// Creates a historical CAS owner binding for a plane in this validated
    /// local generation. The binding is suitable only for local CAS lookup;
    /// it does not recreate the old authority's live selection capability.
    #[must_use]
    pub fn historical_plane_binding(
        &self,
        kind: SemanticPlaneKind,
    ) -> Option<HistoricalSemanticPlaneBinding> {
        if self.image.manifest_root() != self.manifest.root()
            || self.image.semantic_generation() != self.manifest.semantic_generation()
            || self.selected_stamp.profile() != self.manifest.build().profile()
            || !self
                .catalog
                .entries()
                .iter()
                .any(|entry| entry.image() == self.image)
        {
            return None;
        }
        let plane_root = self.manifest.plane(kind)?.root();
        Some(HistoricalSemanticPlaneBinding {
            generation: self.identity,
            stamp: self.selected_stamp,
            image: self.image,
            kind,
            manifest_root: self.manifest.root(),
            plane_root,
        })
    }

    /// Plans a canonical manifest-segment delta from this durable base image.
    ///
    /// The iterator is the semantic crate's ordered versioned-plane merge; it
    /// reports reuse eligibility, fetch, and removal actions without copying
    /// or reimplementing VCS rules. A cold-reopened manifest retains claims
    /// but not coverage capabilities, so the canonical planner may conservatively
    /// return `Fetch` for a byte-identical range. The local CAS still reuses it
    /// after the target segment hash is independently checked. This is a segment
    /// plan, not an entity-level `SemanticDiff`, which requires complete
    /// materialized semantic readers. Callers must admit the target manifest
    /// against a fresh selected catalog before acting on the returned plan.
    pub fn semantic_segment_delta<'base, 'target>(
        &'base self,
        target: &'target SemanticPlaneManifest,
    ) -> Result<SemanticDeltaCursor<'base, 'target>, SemanticManifestError> {
        SemanticDeltaCursor::new(&self.manifest, target, self.manifest.root())
    }
}

/// Private per-target immutable-record and atomic-head file layout.
pub(super) struct LocalSemanticGenerationFiles {
    root: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LocalHead {
    current: LocalSemanticGenerationId,
    previous: Option<LocalSemanticGenerationId>,
    stamp: SelectedGenerationStamp,
}

struct GenerationRecord {
    identity: LocalSemanticGenerationId,
    target: SemanticTargetKey,
    image: SemanticPlaneImageKey,
    image_identity: SemanticImageIdentity,
    catalog: SemanticPlaneCatalog,
    manifest: SemanticPlaneManifest,
}

impl LocalSemanticGenerationFiles {
    pub(super) fn open(root: &Path) -> Result<Self, String> {
        let root = root.join("generations");
        create_private_directory(&root)?;
        set_private_directory(&root)?;
        Ok(Self { root })
    }

    pub(super) fn current(
        &self,
        target: &SemanticTargetKey,
    ) -> Result<Option<LocalSemanticGeneration>, String> {
        let target_root = self.target_root(target);
        if !ensure_optional_directory(&target_root)? {
            return Ok(None);
        }
        // Validate ref catalog and its immediate commit/generation bindings
        // before pruning. Once history exists, retain all metadata records
        // until the indexed mark/sweep contract can prove they are unreachable.
        let may_prune = history::may_prune_generation_records(&target_root, target)?;
        let records_root = target_root.join("records");
        let records_exist = ensure_optional_directory(&records_root)?;
        let head_path = target_root.join("HEAD");
        let Some(head_bytes) = read_optional_bounded(&head_path, MAX_HEAD_BYTES)? else {
            if records_exist && may_prune {
                prune_records(&target_root, None, None, &HashSet::new())?;
            }
            return Ok(None);
        };
        if !records_exist {
            return Err("local semantic generation records directory is missing".to_owned());
        }
        let head = decode_head(&head_bytes)?;
        let record = load_record(&target_root, head.current, target)?;
        validate_record_selection(&record, head.stamp)?;
        let previous_record = head
            .previous
            .map(|previous| load_record(&target_root, previous, target))
            .transpose()?;
        let previous_image = previous_record.as_ref().map(|previous| previous.image);
        let previous_image_identity = previous_record
            .as_ref()
            .map(|previous| previous.image_identity);
        if may_prune {
            prune_records(
                &target_root,
                Some(head.current),
                head.previous,
                &HashSet::new(),
            )?;
        }
        Ok(Some(LocalSemanticGeneration {
            identity: record.identity,
            previous_identity: head.previous,
            previous_image,
            previous_image_identity,
            target: record.target,
            selected_stamp: head.stamp,
            catalog: record.catalog,
            image: record.image,
            image_identity: record.image_identity,
            manifest: record.manifest,
        }))
    }

    pub(super) fn commit<S: SelectedGenerationSource>(
        &self,
        target: &SemanticTargetKey,
        stamp: SelectedGenerationStamp,
        catalog: &SemanticPlaneCatalog,
        image: SemanticPlaneImageKey,
        image_identity: SemanticImageIdentity,
        manifest: &SemanticPlaneManifest,
        source: &mut S,
    ) -> Result<LocalSemanticGeneration, String> {
        self.commit_with_payload_root(
            target,
            stamp,
            catalog,
            image,
            image_identity,
            manifest,
            None,
            source,
        )
    }

    pub(super) fn commit_with_payload_root<S: SelectedGenerationSource>(
        &self,
        target: &SemanticTargetKey,
        stamp: SelectedGenerationStamp,
        catalog: &SemanticPlaneCatalog,
        image: SemanticPlaneImageKey,
        image_identity: SemanticImageIdentity,
        manifest: &SemanticPlaneManifest,
        payload_root: Option<AdmittedHistoryPayloadRoot>,
        source: &mut S,
    ) -> Result<LocalSemanticGeneration, String> {
        let record_bytes =
            encode_generation_record(target, catalog, image, image_identity, manifest)?;
        let record = decode_generation_record(&record_bytes)?;
        validate_record_selection(&record, stamp)?;
        let target_root = self.target_root(target);
        history::recover_pending_retention_delete(&target_root)?;
        let records_root = target_root.join("records");
        create_private_directory(&target_root)?;
        create_private_directory(&records_root)?;
        set_private_directory(&target_root)?;
        set_private_directory(&records_root)?;

        require_current(source, stamp, image)?;
        let head_path = target_root.join("HEAD");
        let prior = read_optional_bounded(&head_path, MAX_HEAD_BYTES)?
            .as_deref()
            .map(decode_head)
            .transpose()?;
        if let Some(prior) = prior {
            let prior_record = load_record(&target_root, prior.current, target)?;
            validate_record_selection(&prior_record, prior.stamp)?;
            if let Some(previous) = prior.previous {
                let _previous_record = load_record(&target_root, previous, target)?;
            }
        }
        self.require_local_cache_head_alignment(
            target,
            prior,
            record.identity,
            stamp,
            record.manifest.root(),
        )?;
        let next = advance_head(prior, record.identity, stamp)?;
        let may_prune = history::may_prune_generation_records(&target_root, target)?;

        // Persist the immutable record first. If the process stops here, HEAD
        // still identifies the prior generation and the orphan is reclaimed
        // by the next open or commit.
        let immutable_path = record_path(&target_root, record.identity);
        match fs::read(&immutable_path) {
            Ok(existing) if existing == record_bytes => {}
            Ok(_) => return Err("immutable semantic generation identity collision".to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bump_generation_records_epoch(&target_root)?;
                backend_platform::durable::write_private_atomic(&immutable_path, &record_bytes)
                    .map_err(display_io)?;
            }
            Err(error) => return Err(display_io(error)),
        }
        #[cfg(test)]
        trip_history_test_fault(HistoryTestFault::AfterGenerationRecord)?;

        let staged_generation = LocalSemanticGeneration {
            identity: record.identity,
            previous_identity: next.previous,
            previous_image: None,
            previous_image_identity: None,
            target: record.target.clone(),
            selected_stamp: stamp,
            catalog: record.catalog.clone(),
            image: record.image,
            image_identity: record.image_identity,
            manifest: record.manifest.clone(),
        };
        // The `local-cache` navigation branch is independent of the external
        // selected index authority and may lead the cache HEAD after a crash.
        // It only names immutable history records; no selection path consults
        // it for authority.
        self.record_selected_history(&staged_generation, source, payload_root)?;

        let head_bytes = encode_head(next)?;
        backend_platform::durable::write_private_atomic(&head_path, &head_bytes)
            .map_err(display_io)?;
        #[cfg(test)]
        trip_history_test_fault(HistoryTestFault::AfterHead)?;
        let previous_record = next
            .previous
            .map(|previous| load_record(&target_root, previous, target))
            .transpose()?;
        let previous_image = previous_record.as_ref().map(|previous| previous.image);
        let previous_image_identity = previous_record
            .as_ref()
            .map(|previous| previous.image_identity);
        if history::may_prune_generation_records(&target_root, target)? {
            prune_records(
                &target_root,
                Some(next.current),
                next.previous,
                &HashSet::new(),
            )?;
        }

        Ok(LocalSemanticGeneration {
            identity: record.identity,
            previous_identity: next.previous,
            previous_image,
            previous_image_identity,
            target: record.target,
            selected_stamp: stamp,
            catalog: record.catalog,
            image: record.image,
            image_identity: record.image_identity,
            manifest: record.manifest,
        })
    }

    fn target_root(&self, target: &SemanticTargetKey) -> PathBuf {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.semantic.local-generation-target.v1\0");
        hash_target(&mut hasher, target);
        self.root.join(hex(hasher.finalize().as_bytes()))
    }
}

fn require_current<S: SelectedGenerationSource>(
    source: &mut S,
    expected: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
) -> Result<(), String> {
    let current = source
        .current_selected_generation()
        .map_err(|error| format!("read current selected semantic generation: {error}"))?;
    if current != expected
        || !source
            .selected_image_is_current(expected, image)
            .map_err(|error| format!("check selected semantic image: {error}"))?
    {
        return Err(
            "selected semantic generation became stale before local head commit".to_owned(),
        );
    }
    Ok(())
}

fn advance_head(
    prior: Option<LocalHead>,
    identity: LocalSemanticGenerationId,
    stamp: SelectedGenerationStamp,
) -> Result<LocalHead, String> {
    let Some(prior) = prior else {
        return Ok(LocalHead {
            current: identity,
            previous: None,
            stamp,
        });
    };
    if prior.stamp.namespace() != stamp.namespace()
        || prior.stamp.profile() != stamp.profile()
        || prior.stamp.source_coordinate() != stamp.source_coordinate()
    {
        return Err("local semantic head belongs to another authority scope".to_owned());
    }
    match stamp
        .selection_revision()
        .cmp(&prior.stamp.selection_revision())
    {
        std::cmp::Ordering::Less => {
            return Err("stale selected authority revision cannot replace local head".to_owned());
        }
        std::cmp::Ordering::Equal => {
            if stamp != prior.stamp || identity != prior.current {
                return Err(
                    "same-revision semantic head conflicts with the selected root".to_owned(),
                );
            }
            return Ok(prior);
        }
        std::cmp::Ordering::Greater => {}
    }
    if identity == prior.current {
        Ok(LocalHead {
            current: identity,
            previous: prior.previous,
            stamp,
        })
    } else {
        Ok(LocalHead {
            current: identity,
            previous: Some(prior.current),
            stamp,
        })
    }
}

fn validate_record_selection(
    record: &GenerationRecord,
    stamp: SelectedGenerationStamp,
) -> Result<(), String> {
    if record.target.profile() != stamp.profile()
        || record.catalog.root() != stamp.catalog_root()
        || record.manifest.root() != record.image.manifest_root()
        || record.manifest.semantic_generation() != record.image.semantic_generation()
        || !record
            .catalog
            .entries()
            .iter()
            .any(|entry| entry.image() == record.image)
    {
        return Err("local semantic generation record differs from its authority head".to_owned());
    }
    Ok(())
}

fn encode_generation_record(
    target: &SemanticTargetKey,
    catalog: &SemanticPlaneCatalog,
    image: SemanticPlaneImageKey,
    image_identity: SemanticImageIdentity,
    manifest: &SemanticPlaneManifest,
) -> Result<Vec<u8>, String> {
    if target.profile() != manifest.build().profile()
        || image.manifest_root() != manifest.root()
        || image.semantic_generation() != manifest.semantic_generation()
    {
        return Err("semantic generation metadata has inconsistent target or image".to_owned());
    }
    let catalog_bytes = catalog.encode().map_err(display_error)?;
    let manifest_bytes = manifest.encode().map_err(display_error)?;
    if catalog_bytes.len() > MAX_CATALOG_BYTES || manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err("semantic generation metadata exceeds its storage bound".to_owned());
    }
    let entry = catalog
        .entries()
        .iter()
        .find(|entry| entry.image() == image)
        .ok_or_else(|| "semantic image is absent from its admitted catalog".to_owned())?;
    if usize::try_from(entry.manifest_length()).ok() != Some(manifest_bytes.len()) {
        return Err("semantic manifest length differs from its catalog entry".to_owned());
    }
    let mut writer = Writer::new(MAX_GENERATION_RECORD_BYTES - CHECKSUM_BYTES - 32);
    writer.header(RECORD_TAG)?;
    writer.target(target)?;
    writer.u32(image.artifact_ordinal())?;
    writer.fixed(image.semantic_generation().as_bytes())?;
    writer.fixed(image.manifest_root().as_bytes())?;
    writer.fixed(image_identity.as_ref())?;
    writer.sized_bytes(&catalog_bytes, MAX_CATALOG_BYTES)?;
    writer.sized_bytes(&manifest_bytes, MAX_MANIFEST_BYTES)?;
    let body = writer.finish();
    let identity = generation_identity(&body);
    let mut output = Vec::with_capacity(body.len() + 32 + CHECKSUM_BYTES);
    output.extend_from_slice(&identity.0);
    output.extend_from_slice(&body);
    append_checksum(&mut output)?;
    Ok(output)
}

fn decode_generation_record(bytes: &[u8]) -> Result<GenerationRecord, String> {
    let body = checked_body(bytes, MAX_GENERATION_RECORD_BYTES)?;
    if body.len() < 32 {
        return Err("semantic generation record is truncated".to_owned());
    }
    let identity = LocalSemanticGenerationId(
        body[..32]
            .try_into()
            .map_err(|_| "semantic generation identity is truncated".to_owned())?,
    );
    let content = &body[32..];
    if generation_identity(content) != identity {
        return Err("semantic generation identity does not match its record".to_owned());
    }
    let mut reader = Reader::new(content);
    reader.header(RECORD_TAG)?;
    let target = reader.target()?;
    let ordinal = reader.u32()?;
    let semantic_generation = GenerationId::from_raw(reader.fixed()?);
    let manifest_root = SemanticManifestRoot::from_wire_claim(reader.fixed()?);
    let image_identity = SemanticImageIdentity::try_from(reader.fixed()?).map_err(display_error)?;
    let catalog_bytes = reader.sized_bytes(MAX_CATALOG_BYTES)?;
    let manifest_bytes = reader.sized_bytes(MAX_MANIFEST_BYTES)?;
    reader.finish()?;
    let catalog = SemanticPlaneCatalog::decode(catalog_bytes).map_err(display_error)?;
    let manifest = SemanticPlaneManifest::decode(manifest_bytes).map_err(display_error)?;
    let image = SemanticPlaneImageKey::new(ordinal, semantic_generation, manifest_root);
    if manifest.root() != manifest_root
        || manifest.semantic_generation() != semantic_generation
        || !catalog.entries().iter().any(|entry| entry.image() == image)
        || catalog
            .entries()
            .iter()
            .find(|entry| entry.image() == image)
            .is_none_or(|entry| entry.manifest_length() as usize != manifest_bytes.len())
    {
        return Err(
            "semantic generation record contains inconsistent canonical metadata".to_owned(),
        );
    }
    Ok(GenerationRecord {
        identity,
        target,
        image,
        image_identity,
        catalog,
        manifest,
    })
}

fn encode_head(head: LocalHead) -> Result<Vec<u8>, String> {
    let mut writer = Writer::new(MAX_HEAD_BYTES - CHECKSUM_BYTES);
    writer.header(HEAD_TAG)?;
    writer.fixed(&head.current.0)?;
    match head.previous {
        Some(previous) => {
            writer.u8(1)?;
            writer.fixed(&previous.0)?;
        }
        None => writer.u8(0)?,
    }
    writer.stamp(head.stamp)?;
    let mut bytes = writer.finish();
    append_checksum(&mut bytes)?;
    Ok(bytes)
}

fn decode_head(bytes: &[u8]) -> Result<LocalHead, String> {
    let body = checked_body(bytes, MAX_HEAD_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HEAD_TAG)?;
    let current = LocalSemanticGenerationId(reader.fixed()?);
    let previous = match reader.u8()? {
        0 => None,
        1 => Some(LocalSemanticGenerationId(reader.fixed()?)),
        _ => return Err("local semantic head has an invalid predecessor marker".to_owned()),
    };
    let stamp = reader.stamp()?;
    reader.finish()?;
    if previous == Some(current) {
        return Err("local semantic head repeats its current generation".to_owned());
    }
    Ok(LocalHead {
        current,
        previous,
        stamp,
    })
}

fn prune_records(
    target_root: &Path,
    current: Option<LocalSemanticGenerationId>,
    previous: Option<LocalSemanticGenerationId>,
    history: &HashSet<LocalSemanticGenerationId>,
) -> Result<(), String> {
    let records = target_root.join("records");
    let history_names = history
        .iter()
        .map(|identity| hex(&identity.0))
        .collect::<HashSet<_>>();
    let mut entries = Vec::new();
    for entry in fs::read_dir(&records).map_err(display_io)? {
        let entry = entry.map_err(display_io)?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "semantic generation filename is not UTF-8".to_owned())?;
        let Some(stem) = name.strip_suffix(".record") else {
            if name.ends_with(".tmp") {
                ensure_regular_file(&path)?;
                entries.push((path, None));
                if entries.len() > MAX_GENERATION_SCAN_MEMBERS {
                    return Err("semantic generation recovery scan exceeds its bound".to_owned());
                }
                continue;
            }
            return Err("semantic generation directory contains an unknown member".to_owned());
        };
        if !is_hex_digest(stem) {
            return Err("semantic generation filename is malformed".to_owned());
        }
        ensure_regular_file(&path)?;
        entries.push((path, Some(stem.to_owned())));
        if entries.len() > MAX_GENERATION_SCAN_MEMBERS {
            return Err("semantic generation recovery scan exceeds its bound".to_owned());
        }
    }
    for (path, stem) in entries {
        let Some(stem) = stem else {
            remove_file(&path)?;
            continue;
        };
        let keep = current.is_some_and(|id| stem == hex(&id.0))
            || previous.is_some_and(|id| stem == hex(&id.0))
            || history_names.contains(&stem);
        if !keep {
            remove_file(&path)?;
        }
    }
    Ok(())
}

fn record_path(target_root: &Path, identity: LocalSemanticGenerationId) -> PathBuf {
    target_root
        .join("records")
        .join(format!("{}.record", hex(&identity.0)))
}

fn bump_generation_records_epoch(target_root: &Path) -> Result<(), String> {
    const MAX_EPOCH_BYTES: usize = 64;
    let path = target_root.join("records.epoch");
    let current = match read_optional_bounded(&path, MAX_EPOCH_BYTES)? {
        Some(bytes) => {
            let body = checked_body(&bytes, MAX_EPOCH_BYTES)?;
            let mut reader = Reader::new(body);
            reader.header(RECORDS_EPOCH_TAG)?;
            let epoch = reader.u64()?;
            reader.finish()?;
            epoch
        }
        None => 0,
    };
    let next = current
        .checked_add(1)
        .ok_or_else(|| "semantic generation-record epoch overflows".to_owned())?;
    let mut writer = Writer::new(MAX_EPOCH_BYTES - CHECKSUM_BYTES);
    writer.header(RECORDS_EPOCH_TAG)?;
    writer.u64(next)?;
    let mut bytes = writer.finish();
    bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
    backend_platform::durable::write_private_atomic(&path, &bytes).map_err(display_io)
}

fn load_record(
    target_root: &Path,
    identity: LocalSemanticGenerationId,
    target: &SemanticTargetKey,
) -> Result<GenerationRecord, String> {
    let path = record_path(target_root, identity);
    let bytes = read_optional_bounded(&path, MAX_GENERATION_RECORD_BYTES)?
        .ok_or_else(|| "local semantic generation head references a missing record".to_owned())?;
    let record = decode_generation_record(&bytes)?;
    #[cfg(test)]
    count_history_generation_decode();
    if record.identity != identity || &record.target != target {
        return Err("local semantic generation head references another target".to_owned());
    }
    Ok(record)
}

fn generation_identity(content: &[u8]) -> LocalSemanticGenerationId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic.local-generation.v1\0");
    hasher.update(content);
    LocalSemanticGenerationId(*hasher.finalize().as_bytes())
}

fn hash_target(hasher: &mut blake3::Hasher, target: &SemanticTargetKey) {
    hasher.update(&(target.package().len() as u64).to_be_bytes());
    hasher.update(target.package().as_bytes());
    hasher.update(&(target.coordinate().len() as u64).to_be_bytes());
    hasher.update(target.coordinate().as_bytes());
    hasher.update(&<[u8; 2]>::from(target.profile()));
}

struct Writer {
    bytes: Vec<u8>,
    maximum: usize,
}

impl Writer {
    fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<(), String> {
        let total = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| "semantic generation record length overflows".to_owned())?;
        if total > self.maximum {
            return Err("semantic generation record exceeds its bound".to_owned());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn header(&mut self, tag: u8) -> Result<(), String> {
        self.push(&MAGIC)?;
        self.u8(VERSION)?;
        self.u8(tag)
    }

    fn target(&mut self, target: &SemanticTargetKey) -> Result<(), String> {
        self.sized_bytes(target.package().as_bytes(), MAX_TARGET_FIELD_BYTES)?;
        self.sized_bytes(target.coordinate().as_bytes(), MAX_TARGET_FIELD_BYTES)?;
        self.push(&<[u8; 2]>::from(target.profile()))
    }

    fn stamp(&mut self, stamp: SelectedGenerationStamp) -> Result<(), String> {
        self.push(stamp.namespace())?;
        self.push(&<[u8; 2]>::from(stamp.profile()))?;
        self.push(stamp.source_coordinate())?;
        self.push(&stamp.selection_revision().to_be_bytes())?;
        self.push(stamp.selected_root())?;
        self.push(stamp.closure_id())?;
        self.push(stamp.catalog_root().as_bytes())
    }

    fn u8(&mut self, value: u8) -> Result<(), String> {
        self.push(&[value])
    }

    fn u32(&mut self, value: u32) -> Result<(), String> {
        self.push(&value.to_be_bytes())
    }

    fn fixed(&mut self, value: &[u8]) -> Result<(), String> {
        self.push(value)
    }

    fn sized_bytes(&mut self, value: &[u8], maximum: usize) -> Result<(), String> {
        if value.len() > maximum {
            return Err("semantic generation field exceeds its bound".to_owned());
        }
        self.u64(u64::try_from(value.len()).map_err(display_error)?)?;
        self.push(value)
    }

    fn u64(&mut self, value: u64) -> Result<(), String> {
        self.push(&value.to_be_bytes())
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| "semantic generation offset overflows".to_owned())?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| "semantic generation record is truncated".to_owned())?;
        self.offset = end;
        Ok(value)
    }

    fn header(&mut self, expected_tag: u8) -> Result<(), String> {
        if self.take(4)? != MAGIC || self.u8()? != VERSION || self.u8()? != expected_tag {
            return Err("semantic generation record header is invalid".to_owned());
        }
        Ok(())
    }

    fn target(&mut self) -> Result<SemanticTargetKey, String> {
        let package = std::str::from_utf8(self.sized_bytes(MAX_TARGET_FIELD_BYTES)?)
            .map_err(|_| "semantic target package is not UTF-8".to_owned())?;
        let coordinate = std::str::from_utf8(self.sized_bytes(MAX_TARGET_FIELD_BYTES)?)
            .map_err(|_| "semantic target coordinate is not UTF-8".to_owned())?;
        let profile = backend_semantic::ir::LanguageProfile::try_from(self.fixed()?)
            .map_err(display_error)?;
        SemanticTargetKey::new(package, coordinate, profile).map_err(display_error)
    }

    fn stamp(&mut self) -> Result<SelectedGenerationStamp, String> {
        let namespace = self.fixed()?;
        let profile = backend_semantic::ir::LanguageProfile::try_from(self.fixed()?)
            .map_err(display_error)?;
        let source_coordinate = self.fixed()?;
        let selection_revision = self.u64()?;
        let selected_root = self.fixed()?;
        let closure_id = self.fixed()?;
        let catalog_root =
            backend_semantic::ir::SemanticPlaneCatalogRoot::from_wire_claim(self.fixed()?);
        SelectedGenerationStamp::checked(
            namespace,
            profile,
            source_coordinate,
            selection_revision,
            selected_root,
            closure_id,
            catalog_root,
        )
        .map_err(display_error)
    }

    fn sized_bytes(&mut self, maximum: usize) -> Result<&'a [u8], String> {
        let length = usize::try_from(self.u64()?)
            .map_err(|_| "semantic generation field exceeds address space".to_owned())?;
        if length > maximum {
            return Err("semantic generation field exceeds its bound".to_owned());
        }
        self.take(length)
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.take(N)?
            .try_into()
            .map_err(|_| "semantic generation field has the wrong width".to_owned())
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.fixed::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.fixed()?))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.fixed()?))
    }

    fn finish(&self) -> Result<(), String> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err("semantic generation record has trailing bytes".to_owned())
        }
    }
}

fn checked_body(bytes: &[u8], maximum: usize) -> Result<&[u8], String> {
    if bytes.len() < CHECKSUM_BYTES + 6 || bytes.len() > maximum {
        return Err("semantic generation record length is invalid".to_owned());
    }
    let body_length = bytes.len() - CHECKSUM_BYTES;
    if blake3::hash(&bytes[..body_length]).as_bytes() != &bytes[body_length..] {
        return Err("semantic generation record checksum failed".to_owned());
    }
    Ok(&bytes[..body_length])
}

fn append_checksum(bytes: &mut Vec<u8>) -> Result<(), String> {
    if bytes.len().saturating_add(CHECKSUM_BYTES) > MAX_GENERATION_RECORD_BYTES {
        return Err("semantic generation record exceeds its bound".to_owned());
    }
    let checksum = blake3::hash(bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    Ok(())
}

fn read_optional_bounded(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(display_io(error)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum as u64 {
        return Err("semantic generation state member is not a bounded regular file".to_owned());
    }
    fs::read(path).map(Some).map_err(display_io)
}

fn ensure_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(display_io)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("semantic generation state path is not a directory".to_owned());
    }
    Ok(())
}

fn ensure_optional_directory(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            ensure_directory(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(display_io(error)),
    }
}

fn create_private_directory(path: &Path) -> Result<(), String> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => ensure_directory(path),
        Err(error) => Err(display_io(error)),
    }
}

fn ensure_regular_file(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(display_io)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("semantic generation state member is not a regular file".to_owned());
    }
    Ok(())
}

fn set_private_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = fs::metadata(path).map_err(display_io)?.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(path, permissions).map_err(display_io)?;
    }
    Ok(())
}

fn remove_file(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => backend_platform::durable::sync_parent(path).map_err(display_io),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(display_io(error)),
    }
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn display_io(error: std::io::Error) -> String {
    error.to_string()
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use backend_semantic::ir::DocInput;
    use backend_semantic::ir::{
        BorrowedTree, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityVersion,
        FactAvailability, GenerationId, ImageProvenance, IrBuilder, ItemKind, LanguageProfile,
        MAX_SEMANTIC_SEGMENT_BYTES, ParentageAuthority, RustEdition, SemanticBuildIdentity,
        SemanticImageAuthority, SemanticImageFacts, SemanticInputClaimV2, SemanticInputWitness,
        SemanticIrPlane, SemanticPlane, SemanticPlaneCatalogEntry, SemanticPlaneKind,
        SemanticPlaneSegment, SemanticRangeRequest, SemanticReader,
        SemanticTypedPlaneFamilyDescriptorV2, SemanticTypedPlaneManifestV2,
        SemanticTypedPlaneVerificationTierV2, TreeItemInput, UntrustedSemanticContentRootV2,
        UntrustedSemanticGenerationRootV2, VariantFingerprint, Visibility,
        encode_full_semantic_image, full_semantic_image_len,
        verify_typed_plane_content_v2_with_tier,
    };
    use backend_semantic::vocabulary::Stage;
    use backend_store::{ArtifactClosureClaim, FileStore, StreamingClosureBudget, TypedObject};
    use backend_version::{Coverage, ObjectKey, ScopeRoot};

    use crate::{
        AdaptiveIrResidency, ByteRange, DurableSemanticRangeStore, DurableSemanticSegmentStore,
        FileSemanticRangeStore, IrResidencyDeltaHop, IrResidencyPath, MAX_RANGE_BYTES,
        SelectedSemanticPlane, TransportLimits,
    };

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "backend-semantic-generation-store-{}-{nonce}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create unique generation fixture directory");
            set_private_directory(&path).expect("private generation fixture directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn persisted_segment_object_id(
        directory: &TestDirectory,
        payload: &[u8],
    ) -> backend_store::ObjectId {
        let store = FileStore::open(directory.0.join("object-cas"), 1024 * 1024)
            .expect("open fixture FileStore");
        let key = ObjectKey::<super::super::ir_hydration_store::SemanticSegmentPayload>::from_value(
            payload,
        );
        let object = TypedObject::from_value(&key, payload);
        store.write_object(&object).expect("persist fixture object")
    }

    struct Fixture {
        target: SemanticTargetKey,
        catalog: SemanticPlaneCatalog,
        image: SemanticPlaneImageKey,
        image_identity: SemanticImageIdentity,
        manifest: SemanticPlaneManifest,
        stamp: SelectedGenerationStamp,
    }

    fn fixture(payload: &[u8], revision: u64, root: u8) -> Fixture {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let target = SemanticTargetKey::new(
            "pkg:cargo/tentpole-app@1.2.3",
            "pkg:cargo/tentpole-app@1.2.3",
            profile,
        )
        .expect("fixture target");
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let segment = SemanticPlaneSegment::from_payload(kind, [1; 32], [1; 32], 1, payload)
            .expect("fixture segment");
        let plane =
            SemanticPlane::claimed(kind, vec![segment], Coverage::Complete).expect("fixture plane");
        let manifest = SemanticPlaneManifest::new(
            GenerationId::from_canonical_bytes(payload),
            SemanticBuildIdentity::new(
                [1; 32],
                [2; 32],
                profile,
                Stage::LowerIr,
                [3; 32],
                [4; 32],
                [5; 32],
                [6; 32],
            ),
            SemanticInputWitness::claimed([7; 32], ScopeRoot::from_bytes([8; 32])),
            vec![plane],
        )
        .expect("fixture manifest");
        let image = SemanticPlaneImageKey::from_manifest(0, &manifest);
        let image_identity = SemanticImageIdentity::from_encoded_bytes(payload);
        let manifest_length = u32::try_from(manifest.encode().expect("manifest encoding").len())
            .expect("small manifest length");
        let catalog = SemanticPlaneCatalog::new(vec![
            SemanticPlaneCatalogEntry::new(image, manifest_length).expect("catalog entry"),
        ])
        .expect("fixture catalog");
        let stamp = SelectedGenerationStamp::checked(
            [9; 16],
            profile,
            [10; 32],
            revision,
            [root; 32],
            [11; 32],
            catalog.root(),
        )
        .expect("fixture selected stamp");
        Fixture {
            target,
            catalog,
            image,
            image_identity,
            manifest,
            stamp,
        }
    }

    fn empty_typed_v2_manifest(input_root: u8) -> SemanticTypedPlaneManifestV2 {
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
        let facts = SemanticImageFacts {
            authority: SemanticImageAuthority::Shared,
            provenance: ImageProvenance::Unavailable,
        };
        let input = SemanticInputClaimV2::from_untrusted_claims(
            [input_root; 32],
            ScopeRoot::from_bytes([8; 32]),
            Coverage::Complete,
        );
        let kinds = [
            SemanticIrPlane::Core,
            SemanticIrPlane::Types,
            SemanticIrPlane::Relations,
            SemanticIrPlane::Occurrences,
            SemanticIrPlane::Documentation,
            SemanticIrPlane::SourceProvenance,
            SemanticIrPlane::LanguageExtensions(profile),
        ];
        let row_index_root = backend_semantic::ir::row_index::StableRowIndex::builder()
            .finish()
            .expect("empty row index")
            .root()
            .as_bytes();
        let family_roots = kinds.map(|family| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"backend.semantic.ir.family-row-index-root.v2\0");
            let family_tag = match family {
                SemanticIrPlane::Core => 1,
                SemanticIrPlane::Types => 2,
                SemanticIrPlane::Relations => 3,
                SemanticIrPlane::Occurrences => 4,
                SemanticIrPlane::Documentation => 5,
                SemanticIrPlane::SourceProvenance => 6,
                SemanticIrPlane::LanguageExtensions(_) => 7,
            };
            hasher.update(&[2, family_tag]);
            if let SemanticIrPlane::LanguageExtensions(profile) = family {
                hasher.update(&<[u8; 2]>::from(profile));
            }
            hasher.update(&0_u64.to_be_bytes());
            hasher.update(&row_index_root);
            *hasher.finalize().as_bytes()
        });
        let mut content_hasher = blake3::Hasher::new_derive_key("backend.semantic.ir.content.v2");
        content_hasher.update(&[2, 0]); // root format and shared-image authority
        content_hasher.update(&7_u16.to_be_bytes());
        for (index, (family, family_root)) in kinds.iter().zip(family_roots).enumerate() {
            content_hasher.update(&[u8::try_from(index).expect("seven family tags")]);
            if let SemanticIrPlane::LanguageExtensions(profile) = family {
                content_hasher.update(&<[u8; 2]>::from(*profile));
            }
            content_hasher.update(&family_root);
            content_hasher.update(&0_u64.to_be_bytes());
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
        let generation_root = *generation_hasher.finalize().as_bytes();
        let descriptors = kinds.map(|family| {
            SemanticTypedPlaneFamilyDescriptorV2::from_untrusted_claims(
                family,
                0,
                backend_semantic::ir::SemanticPlaneSegmentBoundaryPolicy::stable_key_hash_ramp(
                    20, 4096, 1_048_576,
                )
                .expect("fixture boundary policy"),
                Vec::new(),
            )
            .expect("empty typed family descriptor")
        });
        SemanticTypedPlaneManifestV2::from_untrusted_claims(
            build,
            facts,
            input,
            UntrustedSemanticContentRootV2::from_wire_claim(content_root),
            UntrustedSemanticGenerationRootV2::from_wire_claim(generation_root),
            descriptors,
        )
        .expect("canonical empty V2 manifest")
    }

    fn valid_nxfi_fixture(documentation: &str, revision: u64, root: u8) -> (Fixture, Vec<u8>) {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let target = SemanticTargetKey::new(
            "pkg:cargo/tentpole-app@1.2.3",
            "pkg:cargo/tentpole-app@1.2.3",
            profile,
        )
        .expect("fixture target");
        let authority = EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            members: FactAvailability::Captured,
            documentation: FactAvailability::Captured,
            attributes: FactAvailability::Captured,
            visibility: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        };
        let versions = [EntityVersion {
            family: DeclarationFamilyId::from_raw([root; 16]),
            variant: VariantFingerprint::from_raw([root.wrapping_add(1); 16]),
            core_payload: CorePayloadHash::from_raw([root.wrapping_add(2); 16]),
        }];
        let docs = [DocInput::Text(documentation)];
        let items = [TreeItemInput {
            name: b"historical-large-document",
            kind: ItemKind::Module,
            visibility: Visibility::Private,
            authority,
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &docs,
            attributes: &[],
            source: None,
            extension: None,
        }];
        let mut builder = IrBuilder::new();
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &[],
            })
            .expect("build large-document IR");
        let ir = builder.finish().expect("finish large-document IR");
        let image_length = full_semantic_image_len(&ir).expect("plan full semantic image");
        let mut image_bytes = vec![0; image_length];
        encode_full_semantic_image(&ir, &mut image_bytes).expect("encode full semantic image");
        assert!(image_bytes.len() > MAX_SEMANTIC_SEGMENT_BYTES);

        let input = SemanticInputWitness::claimed([7; 32], ScopeRoot::from_bytes([8; 32]));
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let segments = image_bytes
            .chunks(MAX_SEMANTIC_SEGMENT_BYTES)
            .enumerate()
            .map(|(index, payload)| {
                let ordinal = u64::try_from(index).expect("small image segment ordinal");
                let mut key = [0; 32];
                key[24..].copy_from_slice(&ordinal.to_be_bytes());
                SemanticPlaneSegment::from_payload_with_witness(kind, key, key, 1, payload, input)
                    .expect("admit canonical ordinal NXFI segment")
            })
            .collect::<Vec<_>>();
        let plane =
            SemanticPlane::claimed(kind, segments, Coverage::Complete).expect("claim core plane");
        let manifest = SemanticPlaneManifest::new(
            GenerationId::from_canonical_bytes(&image_bytes),
            SemanticBuildIdentity::new(
                [1; 32],
                [2; 32],
                profile,
                Stage::LowerIr,
                [3; 32],
                [4; 32],
                [5; 32],
                [6; 32],
            ),
            input,
            vec![plane],
        )
        .expect("build canonical plane manifest");
        let image = SemanticPlaneImageKey::from_manifest(0, &manifest);
        let image_identity = SemanticImageIdentity::from_encoded_bytes(&image_bytes);
        let manifest_length = u32::try_from(manifest.encode().expect("encode manifest").len())
            .expect("manifest length fits");
        let catalog = SemanticPlaneCatalog::new(vec![
            SemanticPlaneCatalogEntry::new(image, manifest_length).expect("catalog entry"),
        ])
        .expect("build catalog");
        let stamp = SelectedGenerationStamp::checked(
            [9; 16],
            profile,
            [10; 32],
            revision,
            [root; 32],
            [11; 32],
            catalog.root(),
        )
        .expect("selected-generation stamp");
        (
            Fixture {
                target,
                catalog,
                image,
                image_identity,
                manifest,
                stamp,
            },
            image_bytes,
        )
    }

    fn fixture_with_segments(
        generation: u8,
        payloads: &[&[u8]],
        revision: u64,
        root: u8,
    ) -> Fixture {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let target = SemanticTargetKey::new(
            "pkg:cargo/tentpole-app@1.2.3",
            "pkg:cargo/tentpole-app@1.2.3",
            profile,
        )
        .expect("fixture target");
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let input = SemanticInputWitness::claimed([7; 32], ScopeRoot::from_bytes([8; 32]));
        let segments = payloads
            .iter()
            .enumerate()
            .map(|(index, payload)| {
                let first = u8::try_from(index * 2).expect("small fixture index");
                let last = first.checked_add(1).expect("small fixture range");
                SemanticPlaneSegment::from_payload(kind, [first; 32], [last; 32], 1, payload)
                    .expect("fixture segment")
            })
            .collect::<Vec<_>>();
        let plane =
            SemanticPlane::claimed(kind, segments, Coverage::Complete).expect("fixture plane");
        let manifest = SemanticPlaneManifest::new(
            GenerationId::from_raw([generation; 32]),
            SemanticBuildIdentity::new(
                [1; 32],
                [2; 32],
                profile,
                Stage::LowerIr,
                [3; 32],
                [4; 32],
                [5; 32],
                [6; 32],
            ),
            input,
            vec![plane],
        )
        .expect("fixture manifest");
        let image = SemanticPlaneImageKey::from_manifest(0, &manifest);
        let image_identity = SemanticImageIdentity::from_encoded_bytes(b"local generation image");
        let manifest_length = u32::try_from(manifest.encode().expect("manifest encoding").len())
            .expect("small manifest length");
        let catalog = SemanticPlaneCatalog::new(vec![
            SemanticPlaneCatalogEntry::new(image, manifest_length).expect("catalog entry"),
        ])
        .expect("fixture catalog");
        let stamp = SelectedGenerationStamp::checked(
            [9; 16],
            profile,
            [10; 32],
            revision,
            [root; 32],
            [11; 32],
            catalog.root(),
        )
        .expect("fixture selected stamp");
        Fixture {
            target,
            catalog,
            image,
            image_identity,
            manifest,
            stamp,
        }
    }

    fn select_fixture(fixture: &Fixture) -> (TestAuthority, SelectedSemanticPlane) {
        let mut source = TestAuthority::new([fixture.stamp], [fixture.image]);
        let selection = SelectedSemanticPlane::select(
            &mut source,
            &fixture.manifest,
            fixture.image,
            SemanticPlaneKind::Ir(SemanticIrPlane::Core),
        )
        .expect("fresh selected fixture plane");
        (source, selection)
    }

    fn persist_segment(
        store: &mut FileSemanticRangeStore,
        selection: SelectedSemanticPlane,
        manifest: &SemanticPlaneManifest,
        segment: &SemanticPlaneSegment,
        payload: &[u8],
    ) {
        let request = SemanticRangeRequest {
            manifest_root: manifest.root(),
            plane: selection.kind(),
            segment_id: segment.id_claim(),
            first_key: *segment.first_key(),
            last_key: *segment.last_key(),
            byte_length: segment.byte_length(),
        };
        let byte_length = u64::try_from(payload.len()).expect("small payload length");
        store
            .stage_durable_range(
                selection,
                request,
                ByteRange::new(0, byte_length).expect("full segment range"),
                payload,
            )
            .expect("stage FileStore segment");
        let admitted = segment
            .admit(selection.kind(), payload)
            .expect("fixture bytes match segment descriptor");
        let mut admit = |bytes: &[u8]| {
            segment
                .admit(selection.kind(), bytes)
                .map(|_| ())
                .map_err(|_| crate::ReplicationError::IdentityMismatch)
        };
        store
            .commit_and_read(selection, admitted, payload, &mut admit)
            .expect("commit FileStore segment");
    }

    struct TestAuthority {
        observations: VecDeque<SelectedGenerationStamp>,
        current: SelectedGenerationStamp,
        images: Vec<SemanticPlaneImageKey>,
    }

    impl TestAuthority {
        fn new(
            observations: impl IntoIterator<Item = SelectedGenerationStamp>,
            images: impl IntoIterator<Item = SemanticPlaneImageKey>,
        ) -> Self {
            let mut observations = observations.into_iter().collect::<VecDeque<_>>();
            let current = observations
                .pop_front()
                .expect("at least one selected stamp");
            Self {
                observations,
                current,
                images: images.into_iter().collect(),
            }
        }
    }

    impl SelectedGenerationSource for TestAuthority {
        type Error = &'static str;

        fn current_selected_generation(&mut self) -> Result<SelectedGenerationStamp, Self::Error> {
            if let Some(next) = self.observations.pop_front() {
                self.current = next;
            }
            Ok(self.current)
        }

        fn selected_image_is_current(
            &mut self,
            expected_stamp: SelectedGenerationStamp,
            image: SemanticPlaneImageKey,
        ) -> Result<bool, Self::Error> {
            Ok(self.current == expected_stamp && self.images.contains(&image))
        }
    }

    fn commit(
        files: &LocalSemanticGenerationFiles,
        fixture: &Fixture,
        stamps: impl IntoIterator<Item = SelectedGenerationStamp>,
    ) -> Result<LocalSemanticGeneration, String> {
        files.commit(
            &fixture.target,
            fixture.stamp,
            &fixture.catalog,
            fixture.image,
            fixture.image_identity,
            &fixture.manifest,
            &mut TestAuthority::new(stamps, [fixture.image]),
        )
    }

    #[test]
    fn selected_generation_cold_reopens_from_its_checksummed_record() {
        let directory = TestDirectory::create();
        let fixture = fixture(b"full semantic generation one", 1, 12);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let committed = commit(&files, &fixture, [fixture.stamp, fixture.stamp])
            .expect("commit selected generation");
        let local_identity = committed.identity();
        let semantic_generation = committed.semantic_generation();
        assert_ne!(local_identity.as_bytes(), semantic_generation.as_bytes());

        drop(files);
        let reopened_files =
            LocalSemanticGenerationFiles::open(&directory.0).expect("reopen store");
        let reopened = reopened_files
            .current(&fixture.target)
            .expect("read durable head")
            .expect("head exists");
        assert_eq!(reopened.identity(), local_identity);
        assert_eq!(reopened.semantic_generation(), semantic_generation);
        assert_eq!(reopened.selected_stamp(), fixture.stamp);
        assert_eq!(reopened.catalog().root(), fixture.catalog.root());
        assert_eq!(reopened.manifest().root(), fixture.manifest.root());
    }

    #[test]
    fn commit_durability_boundaries_reopen_old_or_new_and_retry_idempotently() {
        let directory = TestDirectory::create();
        let first = fixture(b"durable history first", 1, 91);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let first_commit =
            commit(&files, &first, [first.stamp, first.stamp]).expect("commit baseline generation");
        let head_before = first_commit.identity();

        let second = fixture(b"durable history second", 2, 92);
        arm_history_test_fault(HistoryTestFault::AfterHistoryIndex);
        assert!(
            commit(&files, &second, [second.stamp, second.stamp])
                .expect_err("interrupt after durable history index append")
                .contains("AfterHistoryIndex")
        );
        assert_eq!(
            files
                .current(&second.target)
                .expect("read old HEAD")
                .expect("old HEAD remains")
                .identity(),
            head_before
        );
        let target_root = files.target_root(&second.target);
        let index_path = target_root.join("history").join("commit.index");
        let index_length_after_crash = fs::metadata(&index_path)
            .expect("history index after interruption")
            .len();
        assert!(
            target_root
                .join("history")
                .join("commit.index.intent")
                .exists()
        );
        let second_commit = commit(&files, &second, [second.stamp, second.stamp])
            .expect("recover and retry index append");
        assert_eq!(
            fs::metadata(&index_path)
                .expect("history index after retry")
                .len(),
            index_length_after_crash,
            "retry must not append the same commit index entry twice"
        );
        assert!(
            !target_root
                .join("history")
                .join("commit.index.intent")
                .exists()
        );

        let third = fixture(b"durable history third", 3, 93);
        let commits_root = files
            .target_root(&third.target)
            .join("history")
            .join("commits");
        let commits_before = fs::read_dir(&commits_root)
            .expect("enumerate admitted commits before interrupted admission")
            .map(|entry| {
                entry
                    .expect("read commit directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        arm_history_test_fault(HistoryTestFault::AfterHistoryCommit);
        assert!(
            commit(&files, &third, [third.stamp, third.stamp])
                .expect_err("interrupt after immutable history commit")
                .contains("AfterHistoryCommit")
        );
        assert_eq!(
            files
                .current(&third.target)
                .expect("read HEAD after commit-object interruption")
                .expect("second HEAD remains")
                .identity(),
            second_commit.identity()
        );
        let orphan_commit = fs::read_dir(&commits_root)
            .expect("enumerate commit objects after interrupted admission")
            .map(|entry| {
                entry
                    .expect("read commit directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .find(|name| !commits_before.contains(name))
            .expect("interrupted admission leaves one unindexed commit object");
        drop(files);
        let files = LocalSemanticGenerationFiles::open(&directory.0)
            .expect("cold reopen with an unindexed commit object");
        let mut retention = files
            .advance_history_gc(&third.target)
            .expect("start orphan commit retention after restart");
        while !retention.complete() {
            assert!(
                retention.processed_records() <= history::MAX_HISTORY_GC_BATCH_RECORDS,
                "orphan commit sweep stays within its persisted work budget"
            );
            retention = files
                .advance_history_gc(&third.target)
                .expect("continue orphan commit retention after restart");
        }
        assert!(
            retention.processed_records() <= history::MAX_HISTORY_GC_BATCH_RECORDS,
            "one orphan commit retention cycle stays within its persisted work budget"
        );
        assert!(
            !commits_root.join(&orphan_commit).exists(),
            "unindexed commit objects are reclaimed after a crash"
        );
        let third_commit = commit(&files, &third, [third.stamp, third.stamp])
            .expect("retry immutable history commit and index admission");

        let fourth = fixture(b"durable history fourth", 4, 94);
        arm_history_test_fault(HistoryTestFault::AfterGenerationRecord);
        assert!(
            commit(&files, &fourth, [fourth.stamp, fourth.stamp])
                .expect_err("interrupt after immutable generation record")
                .contains("AfterGenerationRecord")
        );
        assert_eq!(
            files
                .current(&fourth.target)
                .expect("read HEAD after generation-record interruption")
                .expect("third HEAD remains")
                .identity(),
            third_commit.identity()
        );
        let fourth_commit = commit(&files, &fourth, [fourth.stamp, fourth.stamp])
            .expect("retry immutable generation record");

        let fifth = fixture(b"durable history fifth", 5, 95);
        arm_history_test_fault(HistoryTestFault::AfterRefsCatalog);
        assert!(
            commit(&files, &fifth, [fifth.stamp, fifth.stamp])
                .expect_err("interrupt after durable navigation-ref replacement")
                .contains("AfterRefsCatalog")
        );
        let local_cache = HistoryRefName::new("local-cache").expect("local cache ref");
        let ref_ahead = files
            .history_ref(&fifth.target, HistoryRefKind::Branch, &local_cache)
            .expect("read navigation ref after interruption")
            .expect("navigation ref is durable")
            .commit();
        assert_eq!(
            files
                .current(&fifth.target)
                .expect("read cache HEAD before retry")
                .expect("fourth cache HEAD remains")
                .identity(),
            fourth_commit.identity()
        );

        let moved = fixture(b"authority moved after ref write", 6, 96);
        let mut moved_source = TestAuthority::new([moved.stamp], [moved.image]);
        assert!(
            files
                .commit(
                    &fifth.target,
                    fifth.stamp,
                    &fifth.catalog,
                    fifth.image,
                    fifth.image_identity,
                    &fifth.manifest,
                    &mut moved_source,
                )
                .expect_err("a moved source stamp must not adopt the ahead history ref")
                .contains("became stale")
        );
        assert_eq!(
            files
                .current(&fifth.target)
                .expect("read cache HEAD after stale retry")
                .expect("fourth cache HEAD remains")
                .identity(),
            fourth_commit.identity()
        );
        assert_eq!(
            files
                .history_ref(&fifth.target, HistoryRefKind::Branch, &local_cache)
                .expect("ref remains independently navigable")
                .expect("ahead ref remains")
                .commit(),
            ref_ahead
        );
        assert!(
            files
                .commit(
                    &moved.target,
                    moved.stamp,
                    &moved.catalog,
                    moved.image,
                    moved.image_identity,
                    &moved.manifest,
                    &mut TestAuthority::new([moved.stamp, moved.stamp], [moved.image]),
                )
                .expect_err("a different authority generation cannot inherit a ref-ahead crash")
                .contains("ahead of or detached from cache HEAD")
        );
        assert_eq!(
            files
                .current(&fifth.target)
                .expect("cache head remains after lineage divergence")
                .expect("fourth cache head remains")
                .identity(),
            fourth_commit.identity()
        );
        let fifth_commit = commit(&files, &fifth, [fifth.stamp, fifth.stamp])
            .expect("same admitted source deterministically reconciles cache HEAD");
        assert_eq!(
            files
                .history_commit(&fifth.target, ref_ahead)
                .expect("read ref-ahead commit")
                .generation(),
            fifth_commit.identity()
        );

        let sixth = fixture(b"durable history sixth", 7, 97);
        arm_history_test_fault(HistoryTestFault::AfterHead);
        assert!(
            commit(&files, &sixth, [sixth.stamp, sixth.stamp])
                .expect_err("interrupt after durable cache HEAD replacement")
                .contains("AfterHead")
        );
        drop(files);

        let reopened = LocalSemanticGenerationFiles::open(&directory.0).expect("cold reopen");
        assert_eq!(
            reopened
                .current(&sixth.target)
                .expect("read durable new HEAD")
                .expect("new HEAD survived crash boundary")
                .manifest()
                .root(),
            sixth.manifest.root()
        );
        let reopened_tip = reopened
            .history_ref(&sixth.target, HistoryRefKind::Branch, &local_cache)
            .expect("read cold local-cache ref")
            .expect("cold local-cache ref exists")
            .commit();
        let replay = reopened
            .replay_history(&sixth.target, reopened_tip)
            .expect("cold replay after HEAD write");
        assert_eq!(
            replay
                .entries()
                .last()
                .expect("replay has tip")
                .commit()
                .identity(),
            reopened_tip
        );
    }

    #[test]
    fn orphaned_immutable_record_from_interrupted_commit_is_reclaimed() {
        let directory = TestDirectory::create();
        let fixture = fixture(b"generation interrupted before HEAD", 1, 13);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let target_root = files.target_root(&fixture.target);
        let records_root = target_root.join("records");
        fs::create_dir_all(&records_root).expect("create record directory");
        set_private_directory(&target_root).expect("private target directory");
        set_private_directory(&records_root).expect("private record directory");

        let encoded = encode_generation_record(
            &fixture.target,
            &fixture.catalog,
            fixture.image,
            fixture.image_identity,
            &fixture.manifest,
        )
        .expect("encode immutable record");
        let record = decode_generation_record(&encoded).expect("decode immutable record");
        let orphan = record_path(&target_root, record.identity);
        backend_platform::durable::write_private_atomic(&orphan, &encoded)
            .expect("persist record before simulated crash");
        let temporary = records_root.join("interrupted.tmp");
        fs::write(&temporary, b"partial temporary member").expect("write interrupted temp");

        assert!(
            files
                .current(&fixture.target)
                .expect("recover absent HEAD")
                .is_none()
        );
        assert!(!orphan.exists());
        assert!(!temporary.exists());
    }

    #[test]
    fn same_generation_reuses_immutable_identity_while_advancing_authority_stamp() {
        let directory = TestDirectory::create();
        let first = fixture(b"unchanged canonical metadata", 1, 14);
        let second = fixture(b"unchanged canonical metadata", 2, 15);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let first_head =
            commit(&files, &first, [first.stamp, first.stamp]).expect("commit initial head");
        let second_head = commit(&files, &second, [second.stamp, second.stamp])
            .expect("advance authority for same content");

        assert_eq!(second_head.identity(), first_head.identity());
        assert_eq!(second_head.previous_identity(), None);
        assert_eq!(second_head.selected_stamp(), second.stamp);
        let reopened = files
            .current(&second.target)
            .expect("read current head")
            .expect("current head exists");
        assert_eq!(reopened.identity(), first_head.identity());
        assert_eq!(reopened.selected_stamp(), second.stamp);
    }

    #[test]
    fn changed_generation_keeps_one_base_and_rejects_stale_or_same_revision_fork() {
        let directory = TestDirectory::create();
        let base = fixture(b"semantic generation base", 1, 16);
        let target = fixture(b"semantic generation target", 2, 17);
        let stale = fixture(b"stale authority candidate", 1, 18);
        let fork = fixture(b"same revision fork", 2, 19);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");

        let base_head = commit(&files, &base, [base.stamp, base.stamp]).expect("commit base");
        let target_head = commit(&files, &target, [target.stamp, target.stamp])
            .expect("commit changed generation");
        assert_ne!(target_head.identity(), base_head.identity());
        assert_eq!(target_head.previous_identity(), Some(base_head.identity()));

        let stale_result = commit(&files, &stale, [stale.stamp, stale.stamp]);
        assert!(
            stale_result
                .expect_err("lower authority revision is stale")
                .contains("stale selected authority revision")
        );
        let fork_result = commit(&files, &fork, [fork.stamp, fork.stamp]);
        assert!(
            fork_result
                .expect_err("same-revision changed root is a conflict")
                .contains("same-revision semantic head conflicts")
        );

        let current = files
            .current(&target.target)
            .expect("read unchanged current head")
            .expect("current head exists");
        assert_eq!(current.identity(), target_head.identity());
        assert_eq!(current.previous_identity(), Some(base_head.identity()));

        let missing_base = record_path(&files.target_root(&target.target), base_head.identity());
        fs::remove_file(missing_base).expect("remove retained base to test fail-closed reopen");
        assert!(
            files
                .current(&target.target)
                .expect_err("missing predecessor must fail closed")
                .contains("missing record")
        );
    }

    #[test]
    fn cold_reopened_generation_reuses_only_exact_historical_file_segments() {
        let directory = TestDirectory::create();
        let base = fixture_with_segments(
            31,
            &[
                b"unchanged payload",
                b"old changed payload",
                b"deleted payload",
            ],
            31,
            31,
        );
        let target =
            fixture_with_segments(32, &[b"unchanged payload", b"new changed payload"], 32, 32);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let base_segments = base.manifest.plane(kind).expect("base plane").segments();
        let target_segments = target
            .manifest
            .plane(kind)
            .expect("target plane")
            .segments();
        let (mut base_source, base_selection) = select_fixture(&base);
        let (_, target_selection) = select_fixture(&target);
        let cas_root = directory.0.join("cas");
        let cas = FileStore::open(&cas_root, 16 * 1024 * 1024).expect("open fixture FileStore");
        let file_store_limits = TransportLimits {
            max_chunk: 16 * 1024,
            ..TransportLimits::default()
        };
        let mut store = FileSemanticRangeStore::open(cas, file_store_limits)
            .expect("open semantic FileStore adapter");

        for (segment, payload) in base_segments.iter().zip([
            b"unchanged payload".as_slice(),
            b"old changed payload",
            b"deleted payload",
        ]) {
            persist_segment(&mut store, base_selection, &base.manifest, segment, payload);
        }
        let mappings_root = cas_root.join("semantic-hydration/mappings");
        let base_mappings = fs::read_dir(&mappings_root)
            .expect("read base selected mappings")
            .map(|entry| entry.expect("read mapping entry").path())
            .collect::<Vec<_>>();
        assert_eq!(base_mappings.len(), base_segments.len());

        persist_segment(
            &mut store,
            target_selection,
            &target.manifest,
            &target_segments[1],
            b"new changed payload",
        );
        drop(store);

        // Persist the canonical local generation record, then close both the
        // record writer and CAS adapter so the consumer must cold-reopen it.
        let state_root = cas_root.join("semantic-hydration");
        let files = LocalSemanticGenerationFiles::open(&state_root).expect("open generation files");
        let committed = files
            .commit(
                &base.target,
                base.stamp,
                &base.catalog,
                base.image,
                base.image_identity,
                &base.manifest,
                &mut base_source,
            )
            .expect("persist checksummed base generation");
        assert_eq!(committed.image(), base.image);
        drop(files);

        let cas = FileStore::open(&cas_root, 16 * 1024 * 1024).expect("cold reopen FileStore");
        let mut store = FileSemanticRangeStore::open(cas, file_store_limits)
            .expect("cold reopen semantic FileStore adapter");
        let reopened = store
            .current_local_generation(&base.target)
            .expect("reopen checksummed generation head")
            .expect("base generation remains current locally");
        let binding = reopened
            .historical_plane_binding(kind)
            .expect("cold record yields typed historical CAS owner");
        let (mut target_source, target_selection) = select_fixture(&target);
        let chain = [IrResidencyDeltaHop::from_historical(
            reopened.manifest(),
            &target.manifest,
            binding,
        )];
        let mut residency = AdaptiveIrResidency::default();
        let route = residency.prepare_delta_route(target_selection, &target.manifest, &chain);

        let (unchanged_path, unchanged_id) = residency
            .verify_segment_prepared(
                &mut target_source,
                target_selection,
                &target.manifest,
                &target_segments[0],
                &route,
                &mut store,
            )
            .expect("verify exact historical segment against fresh target");
        let IrResidencyPath::DeltaCas(summary) = unchanged_path else {
            panic!("exact unchanged descriptor should use the historical CAS object");
        };
        assert_eq!(
            unchanged_id,
            Some(
                target_segments[0]
                    .admit(kind, b"unchanged payload")
                    .expect("target ID")
            )
        );
        assert_eq!(summary.reused_segments, 1);
        assert_eq!(summary.actions, 3);
        assert_eq!(
            summary.changed_bytes,
            u64::try_from(b"new changed payload".len() + b"deleted payload".len())
                .expect("small changed-byte total")
        );

        let (changed_path, changed_id) = residency
            .verify_segment_prepared(
                &mut target_source,
                target_selection,
                &target.manifest,
                &target_segments[1],
                &route,
                &mut store,
            )
            .expect("verify changed segment from selected target CAS");
        assert_eq!(
            changed_path,
            IrResidencyPath::PristineCas(crate::IrResidencyCasReason::SegmentChanged)
        );
        assert_eq!(
            changed_id,
            Some(
                target_segments[1]
                    .admit(kind, b"new changed payload")
                    .expect("target ID")
            )
        );

        // The historical object can be read only while the freshly selected
        // target stays current through the streaming verification.
        let stale =
            fixture_with_segments(32, &[b"unchanged payload", b"new changed payload"], 33, 33);
        let mut stale_source =
            TestAuthority::new([target.stamp, target.stamp, stale.stamp], [target.image]);
        let mut stale_residency = AdaptiveIrResidency::default();
        assert!(matches!(
            stale_residency.verify_segment_prepared(
                &mut stale_source,
                target_selection,
                &target.manifest,
                &target_segments[0],
                &route,
                &mut store,
            ),
            Err(crate::IrResidencyError::StaleSelection)
        ));

        // A corrupt base mapping must fall back to the independently verified
        // target mapping installed by the successful first reuse.
        for mapping in &base_mappings {
            fs::write(mapping, b"corrupt historical mapping")
                .expect("corrupt only predecessor mappings");
        }
        let mut fallback_residency = AdaptiveIrResidency::default();
        let (fallback_path, fallback_id) = fallback_residency
            .verify_segment_prepared(
                &mut target_source,
                target_selection,
                &target.manifest,
                &target_segments[0],
                &route,
                &mut store,
            )
            .expect("corrupt predecessor falls back to current target mapping");
        assert_eq!(fallback_id, unchanged_id);
        assert_eq!(
            fallback_path,
            IrResidencyPath::PristineCas(crate::IrResidencyCasReason::DeltaCandidateUnavailable)
        );
        assert_eq!(fallback_residency.metrics().unreadable_delta_candidates, 1);
    }

    #[test]
    fn recovery_bound_is_checked_before_any_record_is_removed() {
        let directory = TestDirectory::create();
        let fixture = fixture(b"bounded recovery", 1, 20);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let target_root = files.target_root(&fixture.target);
        let records_root = target_root.join("records");
        fs::create_dir_all(&records_root).expect("create record directory");
        for index in 0..=MAX_GENERATION_SCAN_MEMBERS {
            let path = records_root.join(format!("{:064x}.record", index));
            fs::write(path, b"orphan").expect("write synthetic orphan");
        }

        assert!(
            prune_records(&target_root, None, None, &HashSet::new())
                .expect_err("recovery scan exceeds its bound")
                .contains("exceeds its bound")
        );
        assert_eq!(
            fs::read_dir(records_root)
                .expect("enumerate records")
                .count(),
            MAX_GENERATION_SCAN_MEMBERS + 1
        );
    }

    fn admit_history(
        files: &LocalSemanticGenerationFiles,
        fixture: &Fixture,
        parents: &[HistoryCommitId],
        provenance: [u8; 32],
    ) -> AdmittedHistoryCommit {
        let proposal = files
            .propose_history_commit(&fixture.target, parents, provenance)
            .expect("create unpublished history proposal");
        let mut source = TestAuthority::new([fixture.stamp, fixture.stamp], [fixture.image]);
        files
            .admit_history_proposal(proposal, &mut source)
            .expect("admit immutable history commit")
            .commit()
            .clone()
    }

    fn set_history_ref(
        files: &LocalSemanticGenerationFiles,
        target: &SemanticTargetKey,
        kind: HistoryRefKind,
        name: &str,
        expected: Option<HistoryCommitId>,
        next: Option<HistoryCommitId>,
    ) -> Result<HistoryRefUpdateReceipt, String> {
        files.compare_and_swap_history_ref(target, kind, HistoryRefName::new(name)?, expected, next)
    }

    #[test]
    fn equal_semantic_roots_keep_forks_distinct_and_reject_unmaterialized_merges() {
        let directory = TestDirectory::create();
        let base = fixture(b"same semantic root", 1, 40);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let _ = commit(&files, &base, [base.stamp, base.stamp]).expect("commit selected base");
        let selected_name = HistoryRefName::new("local-cache").expect("local cache ref name");
        let base_commit = files
            .history_ref(&base.target, HistoryRefKind::Branch, &selected_name)
            .expect("read selected ref")
            .expect("selected ref exists")
            .commit();

        let fork = admit_history(&files, &base, &[], [0x71; 32]);
        assert_ne!(fork.identity(), base_commit);
        assert_eq!(fork.manifest_root(), base.manifest.root());
        set_history_ref(
            &files,
            &base.target,
            HistoryRefKind::Branch,
            "other-root",
            None,
            Some(fork.identity()),
        )
        .expect("publish independent root branch");

        let left = admit_history(&files, &base, &[base_commit], [0x99; 32]);
        set_history_ref(
            &files,
            &base.target,
            HistoryRefKind::Branch,
            "left",
            None,
            Some(left.identity()),
        )
        .expect("publish left branch");
        let right = admit_history(&files, &base, &[fork.identity()], [0x99; 32]);
        set_history_ref(
            &files,
            &base.target,
            HistoryRefKind::Branch,
            "right",
            None,
            Some(right.identity()),
        )
        .expect("publish right branch");
        assert_eq!(left.manifest_root(), right.manifest_root());
        assert_ne!(left.identity(), right.identity());
        assert_eq!(left.parents(), &[base_commit]);
        assert_eq!(right.parents(), &[fork.identity()]);
        assert_eq!(
            files
                .propose_history_commit(
                    &base.target,
                    &[left.identity(), right.identity()],
                    [0xaa; 32],
                )
                .expect_err("merge must not advertise a first-parent-only payload closure"),
            HistoryProposalError::UnsupportedMergePayloadClosure
        );
        let mut progress = files
            .advance_history_gc(&base.target)
            .expect("mark every branch root before retention");
        while !progress.complete() {
            progress = files
                .advance_history_gc(&base.target)
                .expect("continue branch-root retention");
        }
        assert_eq!(progress.stats().live_commits(), 4);
        for name in ["local-cache", "other-root", "left", "right"] {
            let reference = files
                .history_ref(
                    &base.target,
                    HistoryRefKind::Branch,
                    &HistoryRefName::new(name).expect("branch name"),
                )
                .expect("read retained branch")
                .expect("branch root remains");
            files
                .history_commit(&base.target, reference.commit())
                .expect("retained branch commit remains readable");
        }
    }

    #[test]
    fn ref_cas_and_rename_are_atomic_and_check_the_expected_value() {
        let directory = TestDirectory::create();
        let base = fixture(b"ref selection", 1, 41);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let committed = commit(&files, &base, [base.stamp, base.stamp]).expect("commit base");
        let local_generation = committed.identity();
        let selected = files
            .history_ref(
                &base.target,
                HistoryRefKind::Branch,
                &HistoryRefName::new("local-cache").expect("local cache name"),
            )
            .expect("read selected")
            .expect("selected exists")
            .commit();
        set_history_ref(
            &files,
            &base.target,
            HistoryRefKind::Tag,
            "release/candidate",
            None,
            Some(selected),
        )
        .expect("create tag");
        assert!(
            set_history_ref(
                &files,
                &base.target,
                HistoryRefKind::Tag,
                "release/candidate",
                None,
                None,
            )
            .expect_err("stale expected ref must fail")
            .contains("compare-and-swap")
        );

        files
            .rename_history_ref(
                &base.target,
                HistoryRefKind::Tag,
                &HistoryRefName::new("release/candidate").expect("old name"),
                HistoryRefName::new("release/v1").expect("new name"),
                selected,
            )
            .expect("atomically rename tag");
        assert!(
            files
                .history_ref(
                    &base.target,
                    HistoryRefKind::Tag,
                    &HistoryRefName::new("release/candidate").expect("old name"),
                )
                .expect("read old tag")
                .is_none()
        );
        assert_eq!(
            files
                .history_ref(
                    &base.target,
                    HistoryRefKind::Tag,
                    &HistoryRefName::new("release/v1").expect("new name"),
                )
                .expect("read renamed tag")
                .expect("renamed tag exists")
                .commit(),
            selected
        );
        assert_eq!(
            files
                .current(&base.target)
                .expect("index selection remains independent")
                .expect("local head exists")
                .identity(),
            local_generation
        );
    }

    #[test]
    fn history_ref_cas_child_process() {
        let Ok(root) = std::env::var("BACKEND_TEST_HISTORY_CAS_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let barrier = root.join("start-cas-race");
        let deadline = Instant::now() + Duration::from_secs(15);
        while !barrier.exists() {
            assert!(
                Instant::now() < deadline,
                "parent did not release CAS barrier"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let expected = decode_history_test_id(
            &std::env::var("BACKEND_TEST_HISTORY_CAS_EXPECTED").expect("expected commit ID"),
        );
        let next = decode_history_test_id(
            &std::env::var("BACKEND_TEST_HISTORY_CAS_NEXT").expect("next commit ID"),
        );
        let worker = std::env::var("BACKEND_TEST_HISTORY_CAS_WORKER").expect("worker name");
        let limits = TransportLimits {
            max_chunk: 16 * 1024,
            ..TransportLimits::default()
        };
        let file_store = FileStore::open(&root, 16 * 1024 * 1024).expect("open child FileStore");
        let sparse =
            FileSemanticRangeStore::open(file_store, limits).expect("open child semantic adapter");
        let target = fixture(b"process CAS base", 1, 111).target;
        let outcome = sparse.compare_and_swap_history_ref(
            &target,
            HistoryRefKind::Branch,
            HistoryRefName::new("race").expect("race ref name"),
            Some(expected),
            Some(next),
        );
        let result = if outcome.is_ok() { "won" } else { "lost" };
        fs::write(root.join(format!("{worker}.result")), result)
            .expect("persist child CAS outcome");
    }

    fn decode_history_test_id(value: &str) -> HistoryCommitId {
        assert_eq!(value.len(), 64, "history commit ID must be 32-byte hex");
        let mut bytes = [0_u8; 32];
        for (slot, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let pair = std::str::from_utf8(pair).expect("history commit ID is ASCII");
            bytes[slot] = u8::from_str_radix(pair, 16).expect("history commit ID is hexadecimal");
        }
        HistoryCommitId::from_bytes(bytes)
    }

    #[test]
    fn two_processes_cannot_both_win_the_same_history_ref_cas() {
        let directory = TestDirectory::create();
        let cas_root = directory.0.join("cas");
        let file_store =
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("open FileStore for process CAS");
        let state_root = cas_root.join("semantic-hydration");
        let files = LocalSemanticGenerationFiles::open(&state_root)
            .expect("open generation store for process CAS");
        let base = fixture(b"process CAS base", 1, 111);
        let _ = commit(&files, &base, [base.stamp, base.stamp]).expect("commit CAS base");
        let local_cache = files
            .history_ref(
                &base.target,
                HistoryRefKind::Branch,
                &HistoryRefName::new("local-cache").expect("local cache ref"),
            )
            .expect("read local-cache ref")
            .expect("local-cache ref exists")
            .commit();
        let left = admit_history(&files, &base, &[local_cache], [0x31; 32]);
        let right = admit_history(&files, &base, &[local_cache], [0x32; 32]);
        let name = HistoryRefName::new("race").expect("race ref name");
        files
            .compare_and_swap_history_ref(
                &base.target,
                HistoryRefKind::Branch,
                name,
                None,
                Some(local_cache),
            )
            .expect("create race ref");
        drop(files);
        drop(file_store);

        let executable = std::env::current_exe().expect("test executable path");
        let child_test = "ir_generation_store::tests::history_ref_cas_child_process";
        let spawn = |worker: &str, next: HistoryCommitId| {
            Command::new(&executable)
                .arg("--exact")
                .arg(child_test)
                .env("BACKEND_TEST_HISTORY_CAS_ROOT", &cas_root)
                .env(
                    "BACKEND_TEST_HISTORY_CAS_EXPECTED",
                    hex(local_cache.as_bytes()),
                )
                .env("BACKEND_TEST_HISTORY_CAS_NEXT", hex(next.as_bytes()))
                .env("BACKEND_TEST_HISTORY_CAS_WORKER", worker)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn competing CAS process")
        };
        let mut left_process = spawn("left", left.identity());
        let mut right_process = spawn("right", right.identity());
        fs::write(cas_root.join("start-cas-race"), b"go").expect("release process CAS barrier");
        assert!(
            left_process
                .wait()
                .expect("wait for left process")
                .success()
        );
        assert!(
            right_process
                .wait()
                .expect("wait for right process")
                .success()
        );
        let left_result =
            fs::read_to_string(cas_root.join("left.result")).expect("read left process result");
        let right_result =
            fs::read_to_string(cas_root.join("right.result")).expect("read right process result");
        assert_ne!(
            left_result, right_result,
            "exactly one process wins the CAS"
        );

        let winner = if left_result == "won" {
            left.identity()
        } else {
            right.identity()
        };
        let reopened = FileSemanticRangeStore::open(
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("reopen FileStore"),
            TransportLimits {
                max_chunk: 16 * 1024,
                ..TransportLimits::default()
            },
        )
        .expect("reopen semantic adapter");
        assert_eq!(
            reopened
                .history_ref(
                    &base.target,
                    HistoryRefKind::Branch,
                    &HistoryRefName::new("race").expect("race ref"),
                )
                .expect("read winner after reopen")
                .expect("race ref remains")
                .commit(),
            winner
        );
    }

    #[test]
    fn cold_reopen_replays_admitted_snapshots_and_borrowed_segment_deltas() {
        let directory = TestDirectory::create();
        let shared = b"stable canonical block shared across three generations";
        let base = fixture_with_segments(51, &[shared, b"base edit"], 1, 51);
        let second = fixture_with_segments(52, &[shared, b"second edit"], 2, 52);
        let third = fixture_with_segments(53, &[shared, b"third edit"], 3, 53);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let _ = commit(&files, &base, [base.stamp, base.stamp]).expect("commit base");
        let _ = commit(&files, &second, [second.stamp, second.stamp]).expect("commit second");
        let second_tip = files
            .history_ref(
                &second.target,
                HistoryRefKind::Branch,
                &HistoryRefName::new("local-cache").expect("local cache ref"),
            )
            .expect("read second-generation ref")
            .expect("second-generation ref exists")
            .commit();
        let _ = commit(&files, &third, [third.stamp, third.stamp]).expect("commit third");
        let feature = admit_history(&files, &third, &[second_tip], [0x54; 32]);
        set_history_ref(
            &files,
            &third.target,
            HistoryRefKind::Branch,
            "feature",
            None,
            Some(feature.identity()),
        )
        .expect("publish edited feature branch");
        let tip = feature.identity();
        drop(files);

        let reopened = LocalSemanticGenerationFiles::open(&directory.0).expect("reopen store");
        let current = reopened
            .current(&third.target)
            .expect("recover selected generation")
            .expect("selected generation remains");
        assert_eq!(current.manifest().root(), third.manifest.root());
        let replay = reopened
            .replay_history(&third.target, tip)
            .expect("read cold ancestry page");
        let roots = replay
            .entries()
            .iter()
            .map(|entry| entry.generation().manifest().root())
            .collect::<Vec<_>>();
        assert_eq!(
            roots,
            vec![
                base.manifest.root(),
                second.manifest.root(),
                third.manifest.root()
            ]
        );
        assert_eq!(replay.segment_deltas().count(), 2);
        assert!(replay.segment_deltas().all(|delta| delta.is_ok()));
        assert_eq!(replay.next_cursor(), None);

        let (mut source, selection) = select_fixture(&third);
        let mut residency = AdaptiveIrResidency::default();
        let mut hop_scratch = Vec::with_capacity(2);
        let route = residency
            .prepare_history_delta_route(selection, &third.manifest, &replay, &mut hop_scratch)
            .expect("build bounded route from checked first-parent replay");
        assert_eq!(hop_scratch.len(), 2);
        let shared_segment = &third
            .manifest
            .plane(selection.kind())
            .expect("core plane")
            .segments()[0];
        assert!(matches!(
            residency
                .plan_with_prepared_route(
                    &mut source,
                    selection,
                    &third.manifest,
                    shared_segment,
                    &route,
                )
                .expect("plan against freshly selected replay tip"),
            IrResidencyPath::DeltaCas(_)
        ));
    }

    #[test]
    fn history_payload_claims_are_admitted_before_gc_or_segment_return_after_cold_reopen() {
        let directory = TestDirectory::create();
        let cas_root = directory.0.join("cas");
        let state_root = cas_root.join("semantic-hydration");
        let file_store =
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("open semantic FileStore");
        let limits = TransportLimits {
            max_chunk: 16 * 1024,
            ..TransportLimits::default()
        };
        let range_store = FileSemanticRangeStore::open(file_store.clone(), limits)
            .expect("open semantic history adapter");
        let generations =
            LocalSemanticGenerationFiles::open(&state_root).expect("open history metadata store");
        let payload = b"claimed historical segment";
        let generation = fixture(payload, 1, 121);
        let plane = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let segment = generation
            .manifest
            .plane(plane)
            .expect("core semantic plane")
            .segments()
            .first()
            .expect("one semantic segment");
        let key = ObjectKey::<super::super::ir_hydration_store::SemanticSegmentPayload>::from_value(
            payload.as_slice(),
        );
        let object = TypedObject::from_value(&key, payload.as_slice());
        let object_id = file_store
            .write_object(&object)
            .expect("persist semantic segment");
        generations
            .persist_history_segment_mapping(
                &generation.target,
                segment.id_claim(),
                object_id,
                segment.byte_length(),
            )
            .expect("persist verified bridge mapping");
        let payload_root = range_store
            .compose_history_payload_root(
                &generation.target,
                &[(segment.id_claim(), object_id, segment.byte_length())],
            )
            .expect("compose payload closure");
        let admitted_root = payload_root.expect("one segment produces a payload closure");
        assert_eq!(
            file_store
                .admit_closure_claim(backend_store::ArtifactClosureClaim::from_id(
                    admitted_root.closure,
                ))
                .expect("admit composed closure claim"),
            admitted_root.closure
        );
        assert!(
            file_store
                .admit_closure_claim(backend_store::ArtifactClosureClaim::from_bytes([0xa5; 32]))
                .is_err()
        );
        let _committed = generations
            .commit_with_payload_root(
                &generation.target,
                generation.stamp,
                &generation.catalog,
                generation.image,
                generation.image_identity,
                &generation.manifest,
                Some(admitted_root),
                &mut TestAuthority::new([generation.stamp, generation.stamp], [generation.image]),
            )
            .expect("admit generation and payload root");
        let ref_name = HistoryRefName::new("local-cache").expect("local-cache ref");
        let history_commit = generations
            .history_ref(&generation.target, HistoryRefKind::Branch, &ref_name)
            .expect("read selected history ref")
            .expect("selected history ref exists")
            .commit();
        let proof = range_store
            .history_ref_ancestry_proof(
                &generation.target,
                HistoryRefKind::Branch,
                &ref_name,
                history_commit,
            )
            .expect("prove selected history commit is reachable");
        let root_path = history::history_payload_root_path(
            &generations.target_root(&generation.target),
            history_commit,
        );
        let valid_root = fs::read(&root_path).expect("read persisted payload root");
        let bridge_path = generations
            .target_root(&generation.target)
            .join("history")
            .join("segment-map")
            .join(format!("{}.map", hex(segment.id_claim().as_bytes())));
        let valid_bridge = fs::read(&bridge_path).expect("read persisted bridge mapping");

        let forged_root = history::encode_test_history_payload_root(
            history_commit,
            HistoryPayloadRoot {
                closure: backend_store::ArtifactClosureClaim::from_bytes([0xa5; 32]),
            },
        )
        .expect("encode forged closure claim");
        fs::remove_file(&root_path).expect("remove valid root before fixture replacement");
        fs::write(&root_path, forged_root).expect("write forged closure claim");
        assert!(
            range_store
                .history_materialization(
                    &generation.target,
                    HistoryRefKind::Branch,
                    &ref_name,
                    history_commit,
                    &proof,
                )
                .is_err(),
            "a forged closure claim must fail before a materialization receipt is returned"
        );
        assert!(
            range_store
                .collect_garbage_with_history(
                    &generation.target,
                    backend_store::GcLimits::default(),
                )
                .is_err(),
            "a forged closure claim must fail before it can become a GC root"
        );
        assert_eq!(
            file_store
                .read_object(object_id)
                .expect("failed GC leaves the historical object available")
                .id(),
            object_id
        );
        fs::remove_file(&root_path).expect("remove forged root");
        fs::write(&root_path, valid_root).expect("restore valid root");

        let impostor_payload = b"unrelated existing object";
        let impostor_key =
            ObjectKey::<super::super::ir_hydration_store::SemanticSegmentPayload>::from_value(
                impostor_payload.as_slice(),
            );
        let impostor = TypedObject::from_value(&impostor_key, impostor_payload.as_slice());
        let impostor_id = file_store
            .write_object(&impostor)
            .expect("persist unrelated object");
        let forged_bridge = history::encode_history_segment_mapping(
            segment.id_claim(),
            impostor_id,
            segment.byte_length(),
        )
        .expect("encode forged object claim");
        fs::remove_file(&bridge_path).expect("remove valid bridge before fixture replacement");
        fs::write(&bridge_path, forged_bridge).expect("write forged object claim");
        drop(generations);
        drop(range_store);
        drop(file_store);

        let reopened_store =
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("cold-open FileStore");
        let reopened = FileSemanticRangeStore::open(reopened_store.clone(), limits)
            .expect("cold-open semantic history adapter");
        let reopened_proof = reopened
            .history_ref_ancestry_proof(
                &generation.target,
                HistoryRefKind::Branch,
                &ref_name,
                history_commit,
            )
            .expect("cold-prove selected history commit is reachable");
        assert!(
            reopened
                .read_history_segment(
                    &generation.target,
                    HistoryRefKind::Branch,
                    &ref_name,
                    history_commit,
                    &reopened_proof,
                    plane,
                    segment.id_claim(),
                )
                .is_err(),
            "an existing object outside the retained closure must not be returned"
        );

        fs::remove_file(&bridge_path).expect("remove forged bridge");
        fs::write(&bridge_path, valid_bridge).expect("restore valid bridge");
        reopened
            .collect_garbage_with_history(&generation.target, backend_store::GcLimits::default())
            .expect("cold-reopened valid closure is admitted as a GC root");
        let mut reader = reopened
            .read_history_segment(
                &generation.target,
                HistoryRefKind::Branch,
                &ref_name,
                history_commit,
                &reopened_proof,
                plane,
                segment.id_claim(),
            )
            .expect("cold-reopened valid segment is returned");
        let mut actual = vec![0; payload.len()];
        assert_eq!(
            reader
                .read_range(0, &mut actual)
                .expect("read cold-reopened segment"),
            payload.len()
        );
        assert_eq!(actual, payload);
    }

    #[test]
    fn named_ref_ancestry_proof_rejects_unrelated_and_stale_tips_after_cold_reopen() {
        let directory = TestDirectory::create();
        let cas_root = directory.0.join("cas");
        let store = FileStore::open(&cas_root, 16 * 1024 * 1024).expect("open FileStore");
        let limits = TransportLimits {
            max_chunk: 16 * 1024,
            ..TransportLimits::default()
        };
        let range_store =
            FileSemanticRangeStore::open(store.clone(), limits).expect("open range store");
        let state_root = cas_root.join("semantic-hydration");
        let files = LocalSemanticGenerationFiles::open(&state_root).expect("open histories");
        let first = fixture(b"ancestry first", 1, 131);
        let second = fixture(b"ancestry second", 2, 132);
        let third = fixture(b"ancestry third", 3, 133);
        let _ = commit(&files, &first, [first.stamp, first.stamp]).expect("commit first");
        let first_commit = files
            .history_ref(
                &first.target,
                HistoryRefKind::Branch,
                &HistoryRefName::new("local-cache").expect("local-cache ref"),
            )
            .expect("read first ref")
            .expect("first ref exists")
            .commit();
        let _ = commit(&files, &second, [second.stamp, second.stamp]).expect("commit second");
        let second_commit = files
            .history_ref(
                &second.target,
                HistoryRefKind::Branch,
                &HistoryRefName::new("local-cache").expect("local-cache ref"),
            )
            .expect("read second ref")
            .expect("second ref exists")
            .commit();
        let _ = commit(&files, &third, [third.stamp, third.stamp]).expect("commit third");
        let tip = files
            .history_ref(
                &third.target,
                HistoryRefKind::Branch,
                &HistoryRefName::new("local-cache").expect("local-cache ref"),
            )
            .expect("read third ref")
            .expect("third ref exists")
            .commit();
        let unrelated = admit_history(&files, &third, &[], [0x91; 32]);
        assert_ne!(unrelated.identity(), first_commit);
        set_history_ref(
            &files,
            &third.target,
            HistoryRefKind::Branch,
            "unrelated",
            None,
            Some(unrelated.identity()),
        )
        .expect("publish unrelated root on same target");
        drop(files);
        drop(range_store);
        drop(store);

        let reopened = FileSemanticRangeStore::open(
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("cold-open FileStore"),
            limits,
        )
        .expect("cold-open range store");
        let local_cache = HistoryRefName::new("local-cache").expect("local-cache ref");
        let proof = reopened
            .history_ref_ancestry_proof(
                &third.target,
                HistoryRefKind::Branch,
                &local_cache,
                first_commit,
            )
            .expect("cold-prove third-old first-parent reachability");
        assert_eq!(proof.ref_tip(), tip);
        assert_eq!(proof.ancestor(), first_commit);
        assert!(
            reopened
                .history_ref_ancestry_proof(
                    &third.target,
                    HistoryRefKind::Branch,
                    &local_cache,
                    unrelated.identity(),
                )
                .expect_err("same-target unrelated commit is not reachable")
                .contains("not reachable")
        );
        assert!(
            reopened
                .history_materialization(
                    &third.target,
                    HistoryRefKind::Branch,
                    &local_cache,
                    unrelated.identity(),
                    &proof,
                )
                .expect_err("proof cannot be reused for another requested commit")
                .contains("does not match")
        );

        reopened
            .compare_and_swap_history_ref(
                &third.target,
                HistoryRefKind::Branch,
                local_cache.clone(),
                Some(tip),
                Some(second_commit),
            )
            .expect("retarget local-cache to a still-descendant tip");
        assert!(
            reopened
                .history_materialization(
                    &third.target,
                    HistoryRefKind::Branch,
                    &local_cache,
                    first_commit,
                    &proof,
                )
                .expect_err("old proof is stale even though its commit stays reachable")
                .contains("moved")
        );
        let fresh_proof = reopened
            .history_ref_ancestry_proof(
                &third.target,
                HistoryRefKind::Branch,
                &local_cache,
                first_commit,
            )
            .expect("fresh proof follows the retargeted ref's first-parent line");
        assert_eq!(fresh_proof.ref_tip(), second_commit);
        assert!(matches!(
            reopened
                .history_materialization(
                    &third.target,
                    HistoryRefKind::Branch,
                    &local_cache,
                    first_commit,
                    &fresh_proof,
                )
                .expect("fresh proof reaches old commit"),
            HistoryMaterialization::NeedsHydration { .. }
        ));
        let first_segment = first
            .manifest
            .plane(SemanticPlaneKind::Ir(SemanticIrPlane::Core))
            .expect("first core plane")
            .segments()
            .first()
            .expect("first core segment")
            .id_claim();
        assert!(matches!(
            reopened.read_history_segment(
                &third.target,
                HistoryRefKind::Branch,
                &local_cache,
                first_commit,
                &proof,
                SemanticPlaneKind::Ir(SemanticIrPlane::Core),
                first_segment,
            ),
            Err(message) if message.contains("moved")
        ));
    }

    #[test]
    fn ancestry_proof_survives_gc_and_same_tip_aba_between_bounded_batches() {
        let directory = TestDirectory::create();
        let cas_root = directory.0.join("cas");
        let store = FileStore::open(&cas_root, 16 * 1024 * 1024).expect("open FileStore");
        let limits = TransportLimits {
            max_chunk: 16 * 1024,
            ..TransportLimits::default()
        };
        let range_store = FileSemanticRangeStore::open(store, limits).expect("open range store");
        let files = LocalSemanticGenerationFiles::open(&cas_root.join("semantic-hydration"))
            .expect("open generation files");
        let generation = fixture(b"ancestry gc batch", 1, 137);
        let _ = commit(&files, &generation, [generation.stamp, generation.stamp])
            .expect("commit base generation");
        let name = HistoryRefName::new("local-cache").expect("local-cache ref");
        let first = files
            .history_ref(&generation.target, HistoryRefKind::Branch, &name)
            .expect("read first ref")
            .expect("first ref exists")
            .commit();
        let mut parent = first;
        for step in 1..=MAX_HISTORY_REPLAY_COMMITS + 1 {
            let mut provenance = [0; 32];
            provenance[0] = u8::try_from(step).expect("bounded fixture step");
            parent = admit_history(&files, &generation, &[parent], provenance).identity();
        }
        let tip = parent;
        range_store
            .compare_and_swap_history_ref(
                &generation.target,
                HistoryRefKind::Branch,
                name.clone(),
                Some(first),
                Some(tip),
            )
            .expect("publish long ancestry tip");
        let unrelated = admit_history(&files, &generation, &[], [0xff; 32]).identity();
        let retained_name = HistoryRefName::new("aba-retained").expect("alternate root ref name");
        range_store
            .compare_and_swap_history_ref(
                &generation.target,
                HistoryRefKind::Branch,
                retained_name.clone(),
                None,
                Some(unrelated),
            )
            .expect("retain unrelated commit under a separate branch");

        let mut gc_batches = 0;
        let proof = range_store
            .history_ref_ancestry_proof_with_batch_hook(
                &generation.target,
                HistoryRefKind::Branch,
                &name,
                first,
                || {
                    if gc_batches == 0 {
                        gc_batches += 1;
                        let gc_calls = {
                            let mut gc_calls = 0;
                            let mut advance_after_cold_open = || {
                                gc_calls += 1;
                                let reopened = FileSemanticRangeStore::open(
                                    FileStore::open(&cas_root, 16 * 1024 * 1024)
                                        .expect("cold-open FileStore for history GC"),
                                    limits,
                                )
                                .expect("cold-open history store between GC batches");
                                reopened
                                    .advance_history_gc(&generation.target)
                                    .expect("GC acquires its exclusive lease between proof batches")
                            };
                            let mut progress = advance_after_cold_open();
                            while !progress.complete() {
                                progress = advance_after_cold_open();
                            }
                            gc_calls
                        };
                        assert!(
                            gc_calls > 1,
                            "the commit index forces a durable retention cursor resume"
                        );
                        assert_eq!(
                            range_store
                                .history_ref(
                                    &generation.target,
                                    HistoryRefKind::Branch,
                                    &retained_name,
                                )
                                .expect("read retained alternate ref after GC")
                                .expect("alternate ref remains rooted through GC")
                                .commit(),
                            unrelated,
                            "the alternate commit remains live through GC"
                        );
                        assert_eq!(
                            range_store
                                .history_commit(&generation.target, unrelated)
                                .expect("read alternate commit after GC")
                                .identity(),
                            unrelated,
                            "the retained alternate commit object survives GC"
                        );
                        range_store
                            .compare_and_swap_history_ref(
                                &generation.target,
                                HistoryRefKind::Branch,
                                name.clone(),
                                Some(tip),
                                Some(unrelated),
                            )
                            .expect("temporarily move the ref away from the proved tip");
                        range_store
                            .compare_and_swap_history_ref(
                                &generation.target,
                                HistoryRefKind::Branch,
                                name.clone(),
                                Some(unrelated),
                                Some(tip),
                            )
                            .expect("restore the same immutable ref tip after ABA");
                    }
                    Ok(())
                },
            )
            .expect("proof resumes after an unpinned history-GC batch");
        assert_eq!(gc_batches, 1, "ancestry walk crosses a batch boundary");
        assert_eq!(proof.ref_tip(), tip);
        assert_eq!(proof.ancestor(), first);
        assert_eq!(
            range_store
                .history_ref(&generation.target, HistoryRefKind::Branch, &name)
                .expect("read ref after interleaved GC")
                .expect("ref remains")
                .commit(),
            tip
        );
    }

    #[test]
    fn third_old_segment_replays_after_forced_file_store_gc_and_cold_reopen() {
        let directory = TestDirectory::create();
        let cas_root = directory.0.join("cas");
        let file_store =
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("open semantic FileStore");
        let limits = TransportLimits {
            max_chunk: 16 * 1024,
            ..TransportLimits::default()
        };
        let mut range_store = FileSemanticRangeStore::open(file_store.clone(), limits)
            .expect("open semantic history adapter");
        let generations = LocalSemanticGenerationFiles::open(&cas_root.join("semantic-hydration"))
            .expect("open generation records");

        let versions = [
            (b"historical segment one".as_slice(), 1_u64, 81_u8),
            (b"historical segment two".as_slice(), 2, 82),
            (b"historical segment three".as_slice(), 3, 83),
        ];
        let mut commits = Vec::new();
        let mut historical_claim = None;
        for (payload, revision, root) in versions {
            let generation = fixture(payload, revision, root);
            let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
            let segment = generation
                .manifest
                .plane(kind)
                .expect("core semantic plane")
                .segments()
                .first()
                .expect("one semantic segment");
            let key =
                ObjectKey::<super::super::ir_hydration_store::SemanticSegmentPayload>::from_value(
                    payload,
                );
            let object = TypedObject::from_value(&key, payload);
            let object_id = file_store
                .write_object(&object)
                .expect("persist immutable semantic segment");
            generations
                .persist_history_segment_mapping(
                    &generation.target,
                    segment.id_claim(),
                    object_id,
                    segment.byte_length(),
                )
                .expect("persist semantic-segment bridge");
            let payload_root = range_store
                .compose_history_payload_root(
                    &generation.target,
                    &[(segment.id_claim(), object_id, segment.byte_length())],
                )
                .expect("compose cumulative history closure");
            if revision == 2 {
                arm_history_test_fault(HistoryTestFault::AfterPayloadRoot);
                assert!(
                    generations
                        .commit_with_payload_root(
                            &generation.target,
                            generation.stamp,
                            &generation.catalog,
                            generation.image,
                            generation.image_identity,
                            &generation.manifest,
                            payload_root,
                            &mut TestAuthority::new(
                                [generation.stamp, generation.stamp],
                                [generation.image],
                            ),
                        )
                        .expect_err("interrupt after durable payload-root publication")
                        .contains("AfterPayloadRoot")
                );
            }
            let _committed = generations
                .commit_with_payload_root(
                    &generation.target,
                    generation.stamp,
                    &generation.catalog,
                    generation.image,
                    generation.image_identity,
                    &generation.manifest,
                    payload_root,
                    &mut TestAuthority::new(
                        [generation.stamp, generation.stamp],
                        [generation.image],
                    ),
                )
                .expect("admit selected generation and local-cache ref");
            let commit_identity = generations
                .history_ref(
                    &generation.target,
                    HistoryRefKind::Branch,
                    &HistoryRefName::new("local-cache").expect("local-cache ref"),
                )
                .expect("read selected history ref")
                .expect("selected history ref exists")
                .commit();
            if commits.is_empty() {
                historical_claim = Some((commit_identity, segment.id_claim(), payload));
            }
            commits.push(commit_identity);
        }
        assert_eq!(commits.len(), 3);
        let history_target = fixture(b"historical segment one", 1, 81).target;
        let mut history_gc = range_store
            .advance_history_gc(&history_target)
            .expect("start retention while the third-old commit is reachable");
        while !history_gc.complete() {
            assert!(
                history_gc.processed_records() <= super::history::MAX_HISTORY_GC_BATCH_RECORDS,
                "history retention stays within its per-call work budget"
            );
            history_gc = range_store
                .advance_history_gc(&history_target)
                .expect("continue bounded retention before payload GC");
        }
        assert!(history_gc.stats().live_commits() >= 3);
        drop(generations);
        drop(range_store);
        drop(file_store);

        let reopened_store =
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("cold-open semantic FileStore");
        let reopened = FileSemanticRangeStore::open(reopened_store, limits)
            .expect("cold-open semantic history adapter");
        let generation = fixture(b"historical segment one", 1, 81);
        let local_cache = HistoryRefName::new("local-cache").expect("local cache ref");
        let proof = reopened
            .history_ref_ancestry_proof(
                &generation.target,
                HistoryRefKind::Branch,
                &local_cache,
                historical_claim.expect("first history claim").0,
            )
            .expect("prove third-old commit remains on selected first-parent line after reopen");
        assert!(matches!(
            reopened
                .history_materialization(
                    &generation.target,
                    HistoryRefKind::Branch,
                    &local_cache,
                    historical_claim.expect("first history claim").0,
                    &proof,
                )
                .expect("verify old manifest segment closure"),
            HistoryMaterialization::ResidentSegments {
                segment_count: 1,
                ..
            }
        ));
        reopened
            .collect_garbage_with_history(&generation.target, backend_store::GcLimits::default())
            .expect("force FileStore collection with history closure roots");

        let reopened_store = FileStore::open(&cas_root, 16 * 1024 * 1024)
            .expect("reopen FileStore after collection");
        let reopened = FileSemanticRangeStore::open(reopened_store, limits)
            .expect("reopen semantic history adapter after collection");
        let (old_commit, old_segment, expected_bytes) =
            historical_claim.expect("first history claim remains available");
        let reopened_proof = reopened
            .history_ref_ancestry_proof(
                &generation.target,
                HistoryRefKind::Branch,
                &local_cache,
                old_commit,
            )
            .expect("reprove third-old ancestry after cold reopen and GC");
        let mut reader = reopened
            .read_history_segment(
                &generation.target,
                HistoryRefKind::Branch,
                &local_cache,
                old_commit,
                &reopened_proof,
                SemanticPlaneKind::Ir(SemanticIrPlane::Core),
                old_segment,
            )
            .expect("read third-old segment after GC and restart");
        let pressure = reopened
            .advance_history_gc(&generation.target)
            .expect_err("a returned historical reader pins metadata during forced GC");
        assert!(
            pressure.contains("deferred"),
            "unexpected GC result: {pressure}"
        );
        assert!(reader.gc_pin_held_for() >= std::time::Duration::ZERO);
        let mut actual = vec![0; expected_bytes.len()];
        assert_eq!(
            reader
                .read_range(0, &mut actual)
                .expect("read verified historical bytes"),
            expected_bytes.len()
        );
        assert_eq!(actual, expected_bytes);
        drop(reader);
        assert!(
            reopened
                .advance_history_gc(&generation.target)
                .expect("GC resumes after historical reader drops")
                .complete()
        );
    }

    #[test]
    fn typed_v2_history_recovers_commit_and_replays_third_old_after_gc_reopen() {
        let directory = TestDirectory::create();
        let cas_root = directory.0.join("cas");
        let file_store =
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("open semantic FileStore");
        let limits = TransportLimits {
            max_chunk: 16 * 1024,
            ..TransportLimits::default()
        };
        let range_store = FileSemanticRangeStore::open(file_store.clone(), limits)
            .expect("open semantic history adapter");
        let generations = LocalSemanticGenerationFiles::open(&cas_root.join("semantic-hydration"))
            .expect("open generation records");
        let generation = fixture(b"typed V2 history selected materialization", 1, 101);
        let _ = commit(
            &generations,
            &generation,
            [generation.stamp, generation.stamp],
        )
        .expect("commit local V1 materialization");
        let closure_receipt = file_store
            .begin_streaming_closure(StreamingClosureBudget::new(1, 1, 1, 16 * 1024, 1, 4 * 1024))
            .expect("begin empty V2 payload closure")
            .seal()
            .expect("seal empty V2 payload closure");
        let closure_claim = ArtifactClosureClaim::from_id(closure_receipt.closure());
        let branch = HistoryRefName::new("typed-v2-history").expect("V2 branch name");

        let stale_stamp = SelectedGenerationStamp::checked(
            *generation.stamp.namespace(),
            generation.stamp.profile(),
            *generation.stamp.source_coordinate(),
            generation
                .stamp
                .selection_revision()
                .checked_add(1)
                .expect("stale test revision does not overflow"),
            *generation.stamp.selected_root(),
            *generation.stamp.closure_id(),
            generation.stamp.catalog_root(),
        )
        .expect("construct a different valid authority stamp");
        let stale_manifest = empty_typed_v2_manifest(70);
        assert!(
            range_store
                .admit_typed_v2_history_commit(
                    &generation.target,
                    &[],
                    [0x70; 32],
                    &stale_manifest,
                    closure_claim,
                    &[],
                    &[],
                    SemanticTypedPlaneVerificationTierV2::Standard,
                    backend_semantic::ir::JumboRopeLimits::default(),
                    &mut TestAuthority::new([stale_stamp], [generation.image]),
                )
                .expect_err("reject a stale selected semantic generation")
                .contains("became stale")
        );
        let target_root = generations.target_root(&generation.target);
        let locator_root = target_root.join("history").join("typed-v2-locators");
        let pending_root = locator_root.join("pending");
        assert_eq!(
            fs::read_dir(&pending_root)
                .expect("read pending locators after stale source")
                .count(),
            0,
            "stale-source rejection clears its pending marker"
        );
        assert_eq!(
            fs::read_dir(&locator_root)
                .expect("read locators after stale source")
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext == "locator")
                })
                .count(),
            0,
            "stale-source rejection removes its uncommitted locator"
        );

        // Leave the durable pending marker and locator exactly where a process
        // crash after sidecar persistence would. Reopening and GC must remove
        // this orphan before ordinary history collection proceeds.
        arm_history_test_fault(HistoryTestFault::AfterTypedV2Locator);
        assert!(
            range_store
                .admit_typed_v2_history_commit(
                    &generation.target,
                    &[],
                    [0x70; 32],
                    &stale_manifest,
                    closure_claim,
                    &[],
                    &[],
                    SemanticTypedPlaneVerificationTierV2::Standard,
                    backend_semantic::ir::JumboRopeLimits::default(),
                    &mut TestAuthority::new(
                        [generation.stamp, generation.stamp],
                        [generation.image],
                    ),
                )
                .expect_err("interrupt after typed V2 locator persistence")
                .contains("AfterTypedV2Locator")
        );
        assert_eq!(
            fs::read_dir(&pending_root)
                .expect("read staged locator marker")
                .count(),
            1
        );
        assert_eq!(
            fs::read_dir(&locator_root)
                .expect("read staged locator")
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext == "locator")
                })
                .count(),
            1
        );
        drop(range_store);
        drop(file_store);
        drop(generations);

        let file_store = FileStore::open(&cas_root, 16 * 1024 * 1024)
            .expect("reopen FileStore after staged locator interruption");
        let range_store = FileSemanticRangeStore::open(file_store.clone(), limits)
            .expect("reopen semantic adapter after staged locator interruption");
        let generations = LocalSemanticGenerationFiles::open(&cas_root.join("semantic-hydration"))
            .expect("reopen generation records after staged locator interruption");
        let mut orphan_gc = range_store
            .advance_history_gc(&generation.target)
            .expect("reconcile orphan typed V2 locator after restart");
        while !orphan_gc.complete() {
            orphan_gc = range_store
                .advance_history_gc(&generation.target)
                .expect("finish GC after orphan locator reconciliation");
        }
        assert_eq!(
            fs::read_dir(&pending_root)
                .expect("read pending locators after recovery GC")
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(&locator_root)
                .expect("read locators after recovery GC")
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext == "locator")
                })
                .count(),
            0
        );

        // A dropped live admission no longer holds the GC pin. Collection may
        // reclaim its unreferenced closure, but a raw commit ID still cannot
        // publish the TypedV2 root and a cold retry must fail closed.
        let unreferenced = range_store
            .admit_typed_v2_history_commit(
                &generation.target,
                &[],
                [0x71; 32],
                &empty_typed_v2_manifest(71),
                closure_claim,
                &[],
                &[],
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
                &mut TestAuthority::new([generation.stamp, generation.stamp], [generation.image]),
            )
            .expect("admit an unreferenced TypedV2 commit");
        let unreferenced_id = unreferenced.commit().identity();
        drop(unreferenced);
        range_store
            .collect_garbage_with_history(&generation.target, backend_store::GcLimits::default())
            .expect("collect the dropped receipt's unreferenced payload closure");
        assert!(
            file_store.open_closure_claim(closure_claim).is_err(),
            "FileStore GC removes the unreferenced payload closure"
        );
        let unreferenced_branch =
            HistoryRefName::new("unreferenced-typed-v2").expect("unreferenced branch name");
        assert!(
            range_store
                .compare_and_swap_history_ref(
                    &generation.target,
                    HistoryRefKind::Branch,
                    unreferenced_branch.clone(),
                    None,
                    Some(unreferenced_id),
                )
                .expect_err("dropped receipt cannot authorize bare-ID publication after GC")
                .contains("proof-bearing admission receipt")
        );
        assert!(
            range_store
                .publish_typed_v2_history_ref_cold(
                    &generation.target,
                    HistoryRefKind::Branch,
                    unreferenced_branch.clone(),
                    None,
                    unreferenced_id,
                    SemanticTypedPlaneVerificationTierV2::Standard,
                    backend_semantic::ir::JumboRopeLimits::default(),
                )
                .expect_err("cold publication cannot recover a collected closure")
                .contains("open cold typed V2 publication closure")
        );
        assert!(
            range_store
                .history_ref(
                    &generation.target,
                    HistoryRefKind::Branch,
                    &unreferenced_branch,
                )
                .expect("read ref after failed publication")
                .is_none()
        );

        let replacement_closure = file_store
            .begin_streaming_closure(StreamingClosureBudget::new(1, 1, 1, 16 * 1024, 1, 4 * 1024))
            .expect("begin replacement empty V2 payload closure")
            .seal()
            .expect("reseal empty V2 payload closure after GC");
        let closure_claim = ArtifactClosureClaim::from_id(replacement_closure.closure());

        let mut previous = None;
        let mut commits = Vec::new();

        for index in 0..3_u8 {
            let manifest = empty_typed_v2_manifest(70 + index);
            let parents = previous.into_iter().collect::<Vec<_>>();
            if index == 0 {
                arm_history_test_fault(HistoryTestFault::AfterHistoryCommit);
                assert!(
                    range_store
                        .admit_typed_v2_history_commit(
                            &generation.target,
                            &parents,
                            [0x70; 32],
                            &manifest,
                            closure_claim,
                            &[],
                            &[],
                            SemanticTypedPlaneVerificationTierV2::Standard,
                            backend_semantic::ir::JumboRopeLimits::default(),
                            &mut TestAuthority::new(
                                [generation.stamp, generation.stamp],
                                [generation.image],
                            ),
                        )
                        .expect_err("interrupt after V2 immutable commit persistence")
                        .contains("AfterHistoryCommit")
                );
            }
            let admission = range_store
                .admit_typed_v2_history_commit(
                    &generation.target,
                    &parents,
                    [0x70 + index; 32],
                    &manifest,
                    closure_claim,
                    &[],
                    &[],
                    SemanticTypedPlaneVerificationTierV2::Standard,
                    backend_semantic::ir::JumboRopeLimits::default(),
                    &mut TestAuthority::new(
                        [generation.stamp, generation.stamp],
                        [generation.image],
                    ),
                )
                .expect("admit or recover V2 history commit");
            let identity = admission.commit().identity();
            if index == 0 {
                crate::ir_hydration_store::reset_typed_v2_closure_reopen_count();
                assert!(
                    range_store
                        .compare_and_swap_history_ref(
                            &generation.target,
                            HistoryRefKind::Branch,
                            branch.clone(),
                            previous,
                            Some(identity),
                        )
                        .expect_err("bare commit ID cannot publish typed V2")
                        .contains("proof-bearing admission receipt")
                );
                assert_eq!(
                    crate::ir_hydration_store::typed_v2_closure_reopen_count(),
                    0,
                    "bare-ID rejection does not reopen the semantic closure"
                );
                arm_history_test_fault(HistoryTestFault::AfterRefsCatalog);
                assert!(
                    range_store
                        .publish_typed_v2_history_ref(
                            &generation.target,
                            HistoryRefKind::Branch,
                            branch.clone(),
                            previous,
                            &admission,
                        )
                        .expect_err("interrupt after durable typed ref CAS")
                        .contains("AfterRefsCatalog")
                );
                assert_eq!(
                    crate::ir_hydration_store::typed_v2_closure_reopen_count(),
                    0,
                    "live receipt retry does not re-read the semantic closure"
                );
            }
            range_store
                .publish_typed_v2_history_ref(
                    &generation.target,
                    HistoryRefKind::Branch,
                    branch.clone(),
                    previous,
                    &admission,
                )
                .expect("publish V2 commit under navigation ref CAS");
            if index == 0 {
                assert_eq!(
                    crate::ir_hydration_store::typed_v2_closure_reopen_count(),
                    0,
                    "idempotent live typed ref retry stays metadata-only"
                );
            }
            drop(admission);
            commits.push(identity);
            previous = Some(identity);
        }

        let cold_tip = commits[2];
        drop(range_store);
        drop(file_store);
        drop(generations);
        let file_store = FileStore::open(&cas_root, 16 * 1024 * 1024)
            .expect("reopen FileStore before cold typed V2 publication retry");
        let range_store = FileSemanticRangeStore::open(file_store.clone(), limits)
            .expect("reopen semantic adapter before cold typed V2 publication retry");
        let generations = LocalSemanticGenerationFiles::open(&cas_root.join("semantic-hydration"))
            .expect("reopen generation records before cold typed V2 publication retry");
        let cold_worker_store = FileSemanticRangeStore::open(
            FileStore::open(&cas_root, 16 * 1024 * 1024)
                .expect("open separate FileStore for cold publication"),
            limits,
        )
        .expect("open separate history adapter for cold publication");
        let writer_store = FileSemanticRangeStore::open(
            FileStore::open(&cas_root, 16 * 1024 * 1024)
                .expect("open separate FileStore for concurrent ref update"),
            limits,
        )
        .expect("open separate history adapter for concurrent ref update");
        let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
        let worker_gate = gate.clone();
        let cold_target = generation.target.clone();
        let cold_branch = branch.clone();
        let cold_worker = std::thread::spawn(move || {
            crate::ir_hydration_store::set_typed_v2_cold_publication_hook(Some(worker_gate));
            crate::ir_hydration_store::reset_typed_v2_closure_reopen_count();
            let result = cold_worker_store.publish_typed_v2_history_ref_cold(
                &cold_target,
                HistoryRefKind::Branch,
                cold_branch,
                Some(cold_tip),
                cold_tip,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            );
            let reopen_count = crate::ir_hydration_store::typed_v2_closure_reopen_count();
            (result, reopen_count)
        });
        // The cold worker is paused after taking its history snapshot and
        // releasing state.lock. An unrelated tag CAS should finish before the
        // cold closure scan is allowed to continue.
        gate.wait();
        let v1_tip = generations
            .history_ref(
                &generation.target,
                HistoryRefKind::Branch,
                &HistoryRefName::new("local-cache").expect("local-cache ref name"),
            )
            .expect("read V1 ref for independent tag")
            .expect("V1 local-cache ref exists")
            .commit();
        let writer_target = generation.target.clone();
        let writer_name =
            HistoryRefName::new("cold-scan-independent-tag").expect("independent tag name");
        let (writer_sender, writer_receiver) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let result = writer_store.compare_and_swap_history_ref(
                &writer_target,
                HistoryRefKind::Tag,
                writer_name,
                None,
                Some(v1_tip),
            );
            let _ = writer_sender.send(result);
        });
        let writer_completed_during_scan = writer_receiver.recv_timeout(Duration::from_secs(5));
        let writer_finished_before_scan_resumed = writer_completed_during_scan.is_ok();
        gate.wait();
        let (cold_result, reopen_count) = cold_worker
            .join()
            .expect("cold publication worker does not panic");
        let writer_result = match writer_completed_during_scan {
            Ok(result) => Some(result),
            Err(_) => writer_receiver.recv_timeout(Duration::from_secs(5)).ok(),
        };
        writer
            .join()
            .expect("independent ref writer does not panic");
        assert!(
            writer_finished_before_scan_resumed
                && writer_result.as_ref().is_some_and(|result| result.is_ok()),
            "independent history ref CAS completes while cold closure scan is paused"
        );
        writer_result
            .expect("writer completed during the cold scan")
            .expect("independent tag CAS succeeds");
        cold_result.expect("cold retry revalidates and publishes idempotently");
        assert!(
            reopen_count > 0,
            "cold publication reopens the durable FileStore closure"
        );

        // A commit locator removed while the cold payload scan is outside
        // state.lock must be detected by the metadata revalidation before
        // the ref CAS. Restore the immutable file afterward for later replay.
        let locator_path =
            locator_root.join(format!("{}.locator", super::hex(cold_tip.as_bytes())));
        let locator_backup = locator_path.with_extension("locator.test-backup");
        let cold_worker_store = FileSemanticRangeStore::open(
            FileStore::open(&cas_root, 16 * 1024 * 1024)
                .expect("open FileStore for metadata revalidation test"),
            limits,
        )
        .expect("open history adapter for metadata revalidation test");
        let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
        let worker_gate = gate.clone();
        let cold_target = generation.target.clone();
        let cold_branch = branch.clone();
        let cold_worker = std::thread::spawn(move || {
            crate::ir_hydration_store::set_typed_v2_cold_publication_hook(Some(worker_gate));
            cold_worker_store.publish_typed_v2_history_ref_cold(
                &cold_target,
                HistoryRefKind::Branch,
                cold_branch,
                Some(cold_tip),
                cold_tip,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
        });
        gate.wait();
        fs::rename(&locator_path, &locator_backup)
            .expect("simulate history GC removing locator during closure scan");
        gate.wait();
        let metadata_result = cold_worker
            .join()
            .expect("metadata revalidation worker does not panic");
        fs::rename(&locator_backup, &locator_path)
            .expect("restore immutable history locator after revalidation test");
        match metadata_result {
            Ok(_) => panic!("cold publication must reject a removed locator"),
            Err(error) => assert!(
                error.contains("typed V2 history locator is missing"),
                "unexpected cold metadata revalidation error: {error}"
            ),
        }

        assert_ne!(commits[0], commits[2]);
        assert!(
            generations
                .history_generation(&generation.target, commits[2])
                .expect_err("V2 cannot be materialized through a V1 generation read")
                .contains("typed V2 history requires proof-bearing cold replay")
        );
        assert!(
            range_store
                .replay_history(&generation.target, commits[2])
                .expect_err("generic V1 replay cannot relabel a V2 commit")
                .contains("typed V2 history requires typed replay")
        );
        let mut history_gc = range_store
            .advance_history_gc(&generation.target)
            .expect("start history mark/sweep with V2 tip");
        while !history_gc.complete() {
            history_gc = range_store
                .advance_history_gc(&generation.target)
                .expect("finish history mark/sweep");
        }
        assert!(
            range_store
                .history_commit(&generation.target, commits[0])
                .is_ok()
        );

        let ancestry = range_store
            .history_ref_ancestry_proof(
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commits[0],
            )
            .expect("prove third-old V2 commit remains reachable");
        let replay = range_store
            .replay_typed_v2_history(
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commits[0],
                &ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("cold-verify third-old V2 commit");
        assert_eq!(replay.commit().identity(), commits[0]);
        assert!(
            replay
                .manifest()
                .content_root_claim()
                .matches(replay.content().content_root())
        );
        drop(replay);

        let _ = range_store
            .collect_garbage_with_history(&generation.target, backend_store::GcLimits::default())
            .expect("collect FileStore with all V2 ancestry closures rooted");
        drop(range_store);
        drop(generations);

        let reopened = FileSemanticRangeStore::open(
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("cold reopen FileStore"),
            limits,
        )
        .expect("cold reopen semantic history adapter");
        let cold_ancestry = reopened
            .history_ref_ancestry_proof(
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commits[0],
            )
            .expect("reprove third-old V2 reachability after cold reopen");
        let cold_replay = reopened
            .replay_typed_v2_history(
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commits[0],
                &cold_ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("replay third-old V2 commit after FileStore GC and restart");
        assert_eq!(cold_replay.commit().identity(), commits[0]);
    }

    #[test]
    fn typed_v2_nonempty_cold_publish_replay_and_gc_after_restart() {
        let directory = TestDirectory::create();
        let cas_root = directory.0.join("cas");
        let limits = TransportLimits {
            max_chunk: 16 * 1024,
            ..TransportLimits::default()
        };
        let file_store =
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("open semantic FileStore");
        let range_store = FileSemanticRangeStore::open(file_store.clone(), limits)
            .expect("open semantic history adapter");
        let generation = fixture(b"typed V2 positive cold publication", 1, 121);
        let generations = LocalSemanticGenerationFiles::open(&cas_root.join("semantic-hydration"))
            .expect("open local generation records");
        let _ = commit(
            &generations,
            &generation,
            [generation.stamp, generation.stamp],
        )
        .expect("commit selected semantic generation");

        let positive = crate::ir_hydration_store::positive_v2_history_fixture_for_test();
        assert_eq!(positive.locator.segments.len(), 6);
        let payload_bytes = positive
            .objects
            .iter()
            .try_fold(0_u64, |total, object| {
                total.checked_add(u64::try_from(object.bytes().len()).ok()?)
            })
            .expect("positive closure payload size fits u64");
        let maximum_object_bytes = positive
            .objects
            .iter()
            .map(|object| object.bytes().len())
            .max()
            .expect("positive closure has segment and rope objects");
        let chunk_bytes = 16 * 1024;
        let chunk_calls = positive
            .objects
            .iter()
            .try_fold(0_usize, |calls, object| {
                let object_calls = object
                    .bytes()
                    .len()
                    .checked_add(chunk_bytes - 1)?
                    .checked_div(chunk_bytes)?;
                calls.checked_add(object_calls)
            })
            .expect("positive closure chunk count fits usize");
        let object_count = positive.objects.len();
        let metadata_bytes = StreamingClosureBudget::metadata_input_bytes_for(object_count)
            .expect("positive closure metadata fits usize");
        let mut builder = file_store
            .begin_streaming_closure(StreamingClosureBudget::new(
                object_count,
                payload_bytes,
                maximum_object_bytes,
                chunk_bytes,
                chunk_calls,
                metadata_bytes,
            ))
            .expect("begin positive typed V2 closure");
        for object in &positive.objects {
            let claim = backend_store::ArtifactObjectClaim::new(
                object.schema(),
                *object.key(),
                *object.version(),
                u64::try_from(object.bytes().len()).expect("fixture object length fits u64"),
            )
            .with_object_id(backend_store::UntrustedObjectId::from_bytes(
                *object.id().as_bytes(),
            ));
            let mut stream = builder
                .begin_object(claim)
                .expect("begin positive closure member");
            for chunk in object.bytes().chunks(chunk_bytes) {
                stream.write(chunk).expect("write positive closure member");
            }
            assert_eq!(
                stream.finish().expect("admit positive closure member"),
                object.id()
            );
        }
        let closure = builder.seal().expect("seal positive typed V2 closure");
        let closure_claim = ArtifactClosureClaim::from_id(closure.closure());
        let admission = range_store
            .admit_typed_v2_history_commit(
                &generation.target,
                &[],
                [0x71; 32],
                &positive.manifest,
                closure_claim,
                &positive.locator.segments,
                &positive.locator.jumbo,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
                &mut TestAuthority::new([generation.stamp, generation.stamp], [generation.image]),
            )
            .expect("admit nonempty seven-family semantic proof");
        let commit_id = admission.commit().identity();
        drop(admission);
        drop(generations);
        drop(range_store);
        drop(file_store);

        // A new FileStore and range-store instance cold-verifies the durable
        // locator and exact object closure before the ref compare-and-swap.
        let cold_file_store =
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("reopen FileStore after restart");
        let cold_range_store = FileSemanticRangeStore::open(cold_file_store.clone(), limits)
            .expect("reopen semantic history adapter after restart");
        let branch = HistoryRefName::new("positive-typed-v2").expect("positive V2 branch name");
        cold_range_store
            .publish_typed_v2_history_ref_cold(
                &generation.target,
                HistoryRefKind::Branch,
                branch.clone(),
                None,
                commit_id,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("cold publish nonempty typed V2 commit");
        assert_eq!(
            cold_range_store
                .history_ref(&generation.target, HistoryRefKind::Branch, &branch)
                .expect("read cold-published branch")
                .expect("branch was published")
                .commit(),
            commit_id
        );

        let same_content_next =
            crate::ir_hydration_store::positive_v2_history_fixture_for_test_with_variants(8, 0);
        assert_eq!(
            same_content_next.expected_content_root,
            positive.expected_content_root
        );
        assert_ne!(
            same_content_next.expected_generation_root,
            positive.expected_generation_root
        );
        assert_eq!(same_content_next.objects.len(), positive.objects.len());
        for (before, after) in positive.objects.iter().zip(&same_content_next.objects) {
            assert_eq!(before.id(), after.id());
            assert_eq!(before.schema(), after.schema());
            assert_eq!(before.bytes(), after.bytes());
        }
        let second_admission = cold_range_store
            .admit_typed_v2_history_commit(
                &generation.target,
                &[commit_id],
                [0x72; 32],
                &same_content_next.manifest,
                closure_claim,
                &same_content_next.locator.segments,
                &same_content_next.locator.jumbo,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
                &mut TestAuthority::new([generation.stamp, generation.stamp], [generation.image]),
            )
            .expect("admit a second exact generation over the shared content closure");
        let second_commit_id = second_admission.commit().identity();
        assert_ne!(commit_id, second_commit_id);
        drop(second_admission);
        cold_range_store
            .publish_typed_v2_history_ref_cold(
                &generation.target,
                HistoryRefKind::Branch,
                branch.clone(),
                Some(commit_id),
                second_commit_id,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("advance branch to second generation with identical content bytes");

        let ancestry = cold_range_store
            .history_ref_ancestry_proof(
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commit_id,
            )
            .expect("prove positive V2 commit is reachable");
        let replay = cold_range_store
            .replay_typed_v2_history(
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commit_id,
                &ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("cold replay published nonempty V2 history");
        assert_eq!(
            replay.content().content_root().as_bytes(),
            &positive.expected_content_root
        );
        assert_eq!(
            replay.content().generation_root().as_bytes(),
            &positive.expected_generation_root
        );
        drop(replay);

        // The nonempty fixture exercises a real seven-family closure and a
        // >1 MiB documentation jumbo. The first request leaves a bounded ghost;
        // a repeated cold request admits owned bytes, and the next is an exact
        // warm hit that avoids locator decode and closure reopening.
        let mut residency = crate::TypedV2HistoryResidencyCache::new(3 * 1024 * 1024, 8)
            .expect("construct bounded residency for nonempty typed V2 closure");
        crate::ir_hydration_store::reset_typed_v2_closure_reopen_count();
        reset_history_replay_load_counts();
        let first_miss = cold_range_store
            .replay_typed_v2_history_with_residency(
                &mut residency,
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commit_id,
                &ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("verify nonempty generation on first cache miss");
        assert!(matches!(
            &first_miss,
            crate::TypedV2HistoryResidencyReplay::Cold(_)
        ));
        drop(first_miss);
        let second_miss = cold_range_store
            .replay_typed_v2_history_with_residency(
                &mut residency,
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commit_id,
                &ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("repeat cold verification and admit nonempty generation");
        assert!(matches!(
            &second_miss,
            crate::TypedV2HistoryResidencyReplay::Resident(_)
        ));
        if let crate::TypedV2HistoryResidencyReplay::Resident(view) = &second_miss {
            assert_eq!(view.commit().identity(), commit_id);
            assert_eq!(
                view.content().content_root().as_bytes(),
                &positive.expected_content_root
            );
            assert_eq!(
                view.content().generation_root().as_bytes(),
                &positive.expected_generation_root
            );
            assert_eq!(view.manifest_bytes(), positive.locator.manifest.as_slice());
            assert_eq!(view.segment_objects(), positive.locator.segments.as_slice());
            assert_eq!(view.jumbo_objects(), positive.locator.jumbo.as_slice());
            assert_eq!(view.payload_byte_len(), payload_bytes);
            assert!(view.payload_byte_len() > 1024 * 1024);
            let mut expected_offset = 0_u64;
            let mut seen_objects = 0_usize;
            for (id, schema, offset, bytes) in view.object_payloads() {
                assert_eq!(offset, expected_offset);
                let original = positive
                    .objects
                    .iter()
                    .find(|object| object.id() == id)
                    .expect("resident object comes from the positive fixture");
                assert_eq!(schema, original.schema());
                assert_eq!(bytes, original.bytes());
                expected_offset = expected_offset
                    .checked_add(u64::try_from(bytes.len()).expect("payload length fits u64"))
                    .expect("fixture offset does not overflow");
                seen_objects += 1;
            }
            assert_eq!(seen_objects, object_count);
            assert_eq!(expected_offset, payload_bytes);
        }
        drop(second_miss);
        assert_eq!(history_replay_load_counts().locator_decodes, 2);
        assert_eq!(
            crate::ir_hydration_store::typed_v2_closure_reopen_count(),
            2
        );

        crate::ir_hydration_store::reset_typed_v2_closure_reopen_count();
        reset_history_replay_load_counts();
        let warm = cold_range_store
            .replay_typed_v2_history_with_residency(
                &mut residency,
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commit_id,
                &ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("serve exact nonempty generation from live owned bytes");
        assert!(matches!(
            &warm,
            crate::TypedV2HistoryResidencyReplay::Resident(_)
        ));
        assert_eq!(history_replay_load_counts().locator_decodes, 0);
        assert_eq!(
            crate::ir_hydration_store::typed_v2_closure_reopen_count(),
            0
        );
        drop(warm);
        let metrics = residency.metrics();
        assert!(metrics.accounted_bytes <= metrics.byte_budget);
        assert_eq!(metrics.entries, 1);
        assert_eq!(metrics.resident_object_bytes, payload_bytes);
        assert!(metrics.transient_high_water_bytes > metrics.accounted_bytes);

        let second_ancestry = cold_range_store
            .history_ref_ancestry_proof(
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                second_commit_id,
            )
            .expect("prove second exact generation is reachable");
        let shared_generation = cold_range_store
            .replay_typed_v2_history_with_residency(
                &mut residency,
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                second_commit_id,
                &second_ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("fully reverify second generation using shared resident object bytes");
        assert!(matches!(
            &shared_generation,
            crate::TypedV2HistoryResidencyReplay::Resident(_)
        ));
        if let crate::TypedV2HistoryResidencyReplay::Resident(view) = &shared_generation {
            assert_eq!(view.commit().identity(), second_commit_id);
            assert_eq!(
                view.content().content_root().as_bytes(),
                &same_content_next.expected_content_root
            );
            assert_eq!(
                view.content().generation_root().as_bytes(),
                &same_content_next.expected_generation_root
            );
            assert_eq!(
                view.manifest_bytes(),
                same_content_next.locator.manifest.as_slice()
            );
            assert_eq!(
                view.segment_objects(),
                same_content_next.locator.segments.as_slice()
            );
            assert_eq!(
                view.jumbo_objects(),
                same_content_next.locator.jumbo.as_slice()
            );
            assert_eq!(view.payload_byte_len(), payload_bytes);
        }
        drop(shared_generation);
        let shared_metrics = residency.metrics();
        assert_eq!(shared_metrics.entries, 2);
        assert_eq!(shared_metrics.payload_bytes_read, 2 * payload_bytes);
        assert_eq!(shared_metrics.resident_payload_bytes_reused, payload_bytes);
        assert_eq!(shared_metrics.resident_object_bytes, payload_bytes);
        assert_eq!(
            shared_metrics.generation_wide_payload_bytes,
            2 * payload_bytes
        );
        assert_eq!(shared_metrics.deduplicated_payload_bytes, payload_bytes);
        assert_eq!(
            shared_metrics.shared_object_reuses,
            u64::try_from(object_count).expect("object count fits u64")
        );
        assert!(shared_metrics.accounted_bytes <= shared_metrics.byte_budget);

        // Race a warm hit between its first ancestry check and its final tip
        // check. A ref move invalidates the request; the live GC reader pin
        // makes bounded history GC defer while the request is paused.
        let warm_gate = std::sync::Arc::new(std::sync::Barrier::new(2));
        let worker_gate = warm_gate.clone();
        let worker_store = FileSemanticRangeStore::open(cold_file_store.clone(), limits)
            .expect("open independent adapter for concurrent warm lookup");
        let worker_target = generation.target.clone();
        let worker_branch = branch.clone();
        let worker_ancestry = ancestry.clone();
        let cache_for_worker = std::mem::replace(
            &mut residency,
            crate::TypedV2HistoryResidencyCache::new(3 * 1024 * 1024, 8)
                .expect("construct temporary cache while worker owns resident entry"),
        );
        let warm_worker = std::thread::spawn(move || {
            let mut residency = cache_for_worker;
            crate::ir_hydration_store::set_typed_v2_residency_warm_recheck_hook(Some(worker_gate));
            let result = worker_store.replay_typed_v2_history_with_residency(
                &mut residency,
                &worker_target,
                HistoryRefKind::Branch,
                &worker_branch,
                commit_id,
                &worker_ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            );
            let outcome = match result {
                Ok(replay) => {
                    let resident =
                        matches!(&replay, crate::TypedV2HistoryResidencyReplay::Resident(_));
                    drop(replay);
                    if resident {
                        Ok(())
                    } else {
                        Err("warm lookup unexpectedly fell back to cold replay".to_owned())
                    }
                }
                Err(error) => Err(error),
            };
            (residency, outcome)
        });
        warm_gate.wait();
        cold_range_store
            .compare_and_swap_history_ref(
                &generation.target,
                HistoryRefKind::Branch,
                branch.clone(),
                Some(second_commit_id),
                None,
            )
            .expect("remove branch tip between warm ancestry checks");
        let deferred_gc = cold_range_store
            .advance_history_gc(&generation.target)
            .expect_err("reader pin defers history GC while warm check is paused");
        assert!(deferred_gc.contains("shared readers hold collection pins"));
        warm_gate.wait();
        let (returned_cache, warm_result) = warm_worker
            .join()
            .expect("concurrent warm lookup worker does not panic");
        residency = returned_cache;
        assert!(
            warm_result
                .expect_err("warm lookup rejects a moved ref tip")
                .contains("history reference moved")
        );
        cold_range_store
            .compare_and_swap_history_ref(
                &generation.target,
                HistoryRefKind::Branch,
                branch.clone(),
                None,
                Some(second_commit_id),
            )
            .expect("restore branch tip after the warm-ref race");

        // A cache hit is bound to the exact previously verified locator bytes
        // and may remain usable if its sidecar is externally replaced. A cold
        // cache must parse that sidecar and reject the damaged envelope.
        let generation_files =
            LocalSemanticGenerationFiles::open(&cas_root.join("semantic-hydration"))
                .expect("open generation records to locate locator sidecar");
        let locator_path = generation_files
            .target_root(&generation.target)
            .join("history")
            .join("typed-v2-locators")
            .join(format!("{}.locator", super::hex(commit_id.as_bytes())));
        let original_locator = fs::read(&locator_path).expect("read typed V2 locator bytes");
        let mut replaced_locator = original_locator.clone();
        *replaced_locator
            .last_mut()
            .expect("locator bytes are nonempty") ^= 1;
        fs::write(&locator_path, &replaced_locator).expect("replace locator with damaged bytes");
        let mut cold_after_locator_replacement =
            crate::TypedV2HistoryResidencyCache::new(3 * 1024 * 1024, 8)
                .expect("construct cold cache for locator replacement");
        reset_history_replay_load_counts();
        let cold_locator_error = cold_range_store
            .replay_typed_v2_history_with_residency(
                &mut cold_after_locator_replacement,
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commit_id,
                &ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect_err("cold cache rejects a replaced locator sidecar");
        assert!(cold_locator_error.contains("typed V2 history locator"));
        assert_eq!(history_replay_load_counts().locator_decodes, 1);
        reset_history_replay_load_counts();
        let warm_with_replaced_locator = cold_range_store
            .replay_typed_v2_history_with_residency(
                &mut residency,
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commit_id,
                &ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("exact live entry uses its owned verified locator bytes");
        assert!(matches!(
            &warm_with_replaced_locator,
            crate::TypedV2HistoryResidencyReplay::Resident(_)
        ));
        assert_eq!(history_replay_load_counts().locator_decodes, 0);
        drop(warm_with_replaced_locator);
        fs::write(&locator_path, &original_locator).expect("restore exact locator sidecar");

        // Verify the physical-byte boundary too. A live cache owns bytes that
        // were fully FileStore-verified on admission; a cold process must
        // re-read and reject later physical-file damage.
        let first_object = positive
            .objects
            .first()
            .expect("positive payload closure is nonempty");
        let object_path = cas_root.join("objects").join(format!(
            "{}.object",
            super::hex(first_object.id().as_bytes())
        ));
        let original_object = fs::read(&object_path).expect("read object before corruption");
        let mut damaged_object = original_object.clone();
        *damaged_object
            .last_mut()
            .expect("object envelope is nonempty") ^= 0x80;
        fs::write(&object_path, &damaged_object).expect("damage physical object bytes");
        reset_history_replay_load_counts();
        let warm_with_damaged_object = cold_range_store
            .replay_typed_v2_history_with_residency(
                &mut residency,
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commit_id,
                &ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("warm cache serves its immutable verified object copy");
        assert!(matches!(
            &warm_with_damaged_object,
            crate::TypedV2HistoryResidencyReplay::Resident(_)
        ));
        assert_eq!(history_replay_load_counts().locator_decodes, 0);
        drop(warm_with_damaged_object);
        let mut cold_after_object_damage =
            crate::TypedV2HistoryResidencyCache::new(3 * 1024 * 1024, 8)
                .expect("construct cold cache for physical-byte corruption");
        let cold_object_error = cold_range_store
            .replay_typed_v2_history_with_residency(
                &mut cold_after_object_damage,
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commit_id,
                &ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect_err("cold replay revalidates physical bytes after restart");
        assert!(cold_object_error.contains("verify typed V2 history object"));
        fs::write(&object_path, &original_object).expect("restore physical object bytes");

        let pinned = cold_range_store
            .replay_typed_v2_history_with_residency(
                &mut residency,
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commit_id,
                &ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("acquire resident view and fresh GC pin");
        let pinned_gc = cold_range_store
            .advance_history_gc(&generation.target)
            .expect_err("resident view keeps a fresh FileStore GC lease");
        assert!(pinned_gc.contains("shared readers hold collection pins"));
        drop(pinned);
        let mut progress = cold_range_store
            .advance_history_gc(&generation.target)
            .expect("GC resumes after resident view drops");
        while !progress.complete() {
            progress = cold_range_store
                .advance_history_gc(&generation.target)
                .expect("complete bounded history GC after pin release");
        }
        assert!(residency.metrics().accounted_bytes <= residency.metrics().byte_budget);

        cold_range_store
            .collect_garbage_with_history(&generation.target, backend_store::GcLimits::default())
            .expect("collect with the positive V2 branch as a closure root");
        drop(cold_range_store);
        drop(cold_file_store);

        let reopened_file_store =
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("reopen FileStore after GC");
        let reopened_range_store = FileSemanticRangeStore::open(reopened_file_store, limits)
            .expect("reopen semantic store after GC");
        let reopened_ancestry = reopened_range_store
            .history_ref_ancestry_proof(
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commit_id,
            )
            .expect("reprove positive V2 reachability after GC and restart");
        let reopened_replay = reopened_range_store
            .replay_typed_v2_history(
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commit_id,
                &reopened_ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("replay positive typed V2 history after GC and restart");
        assert_eq!(
            reopened_replay.content().content_root().as_bytes(),
            &positive.expected_content_root
        );
        assert_eq!(
            reopened_replay.content().generation_root().as_bytes(),
            &positive.expected_generation_root
        );
        drop(reopened_replay);
        let mut cold_restart_residency =
            crate::TypedV2HistoryResidencyCache::new(3 * 1024 * 1024, 8)
                .expect("construct empty residency after process-style restart");
        assert_eq!(cold_restart_residency.metrics().entries, 0);
        reset_history_replay_load_counts();
        crate::ir_hydration_store::reset_typed_v2_closure_reopen_count();
        let cold_restart = reopened_range_store
            .replay_typed_v2_history_with_residency(
                &mut cold_restart_residency,
                &generation.target,
                HistoryRefKind::Branch,
                &branch,
                commit_id,
                &reopened_ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                backend_semantic::ir::JumboRopeLimits::default(),
            )
            .expect("cold restart verifies durable bytes rather than retaining process proof");
        assert!(matches!(
            &cold_restart,
            crate::TypedV2HistoryResidencyReplay::Cold(_)
        ));
        assert_eq!(history_replay_load_counts().locator_decodes, 1);
        assert_eq!(
            crate::ir_hydration_store::typed_v2_closure_reopen_count(),
            1
        );
        assert_eq!(
            cold_restart_residency.metrics().payload_bytes_read,
            payload_bytes
        );
    }

    #[test]
    fn third_old_nxfi_checkout_survives_image_prune_file_store_gc_and_cold_reopen() {
        let directory = TestDirectory::create();
        let cas_root = directory.0.join("cas");
        let state_root = cas_root.join("semantic-hydration");
        let file_store =
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("open semantic FileStore");
        let limits = TransportLimits {
            max_chunk: 16 * 1024,
            ..TransportLimits::default()
        };
        let range_store = FileSemanticRangeStore::open(file_store.clone(), limits)
            .expect("open semantic history adapter");
        let generations =
            LocalSemanticGenerationFiles::open(&state_root).expect("open generation records");
        let images = super::super::ir_image_store::LocalSemanticImageFiles::open(&state_root)
            .expect("open local image cache");

        let mut cached_images = Vec::new();
        let mut first_snapshot = None;
        let mut third_snapshot = None;
        for (revision, root) in [(1_u64, 91_u8), (2, 92), (3, 93)] {
            let documentation = format!("revision-{revision}: {}", "x".repeat(1_100_000));
            let (generation, image_bytes) = valid_nxfi_fixture(&documentation, revision, root);
            let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
            let plane = generation.manifest.plane(kind).expect("core plane");
            let mut segment_objects = Vec::with_capacity(plane.segments().len());
            let mut image_offset = 0_usize;
            for segment in plane.segments() {
                let byte_length = usize::try_from(segment.byte_length())
                    .expect("segment byte length fits address space");
                let image_end = image_offset
                    .checked_add(byte_length)
                    .expect("segment image range fits");
                let payload = &image_bytes[image_offset..image_end];
                let key = ObjectKey::<
                    super::super::ir_hydration_store::SemanticSegmentPayload,
                >::from_value(payload);
                let object = TypedObject::from_value(&key, payload);
                let object_id = file_store
                    .write_object(&object)
                    .expect("persist canonical NXFI segment object");
                generations
                    .persist_history_segment_mapping(
                        &generation.target,
                        segment.id_claim(),
                        object_id,
                        segment.byte_length(),
                    )
                    .expect("persist history segment bridge");
                segment_objects.push((segment.id_claim(), object_id, segment.byte_length()));
                image_offset = image_end;
            }
            assert_eq!(image_offset, image_bytes.len());

            let payload_root = range_store
                .compose_history_payload_root(&generation.target, &segment_objects)
                .expect("compose cumulative segment closure");
            let committed = generations
                .commit_with_payload_root(
                    &generation.target,
                    generation.stamp,
                    &generation.catalog,
                    generation.image,
                    generation.image_identity,
                    &generation.manifest,
                    payload_root,
                    &mut TestAuthority::new(
                        [generation.stamp, generation.stamp],
                        [generation.image],
                    ),
                )
                .expect("admit canonical selected generation");
            let branch = generations
                .history_ref(
                    &generation.target,
                    HistoryRefKind::Branch,
                    &HistoryRefName::new("local-cache").expect("local-cache ref"),
                )
                .expect("read local-cache branch")
                .expect("local-cache branch exists");
            if revision == 1 {
                range_store
                    .compare_and_swap_history_ref(
                        &generation.target,
                        HistoryRefKind::Tag,
                        HistoryRefName::new("old-v1").expect("historical tag"),
                        None,
                        Some(branch.commit()),
                    )
                    .expect("pin first generation under a tag");
            }

            let retained_keys = cached_images
                .iter()
                .map(|(image, _identity)| *image)
                .collect::<Vec<_>>();
            let mut resume = None;
            let total_length = u64::try_from(image_bytes.len()).expect("image length fits");
            for (index, payload) in image_bytes.chunks(MAX_RANGE_BYTES).enumerate() {
                let offset = u64::try_from(index)
                    .expect("image page ordinal fits")
                    .checked_mul(MAX_RANGE_BYTES as u64)
                    .expect("page offset fits");
                let byte_range = ByteRange::new(
                    offset,
                    u64::try_from(payload.len()).expect("page length fits"),
                )
                .expect("valid image page range");
                resume = Some(
                    images
                        .stage_page(
                            &generation.target,
                            generation.image,
                            generation.image_identity,
                            total_length,
                            byte_range,
                            payload,
                            &retained_keys,
                        )
                        .expect("stage a bounded image page"),
                );
            }
            let mapped = images
                .finish(
                    &generation.target,
                    resume.expect("image has at least one page"),
                    &cached_images,
                )
                .expect("admit local canonical image");
            assert_eq!(mapped.identity(), generation.image_identity);
            drop(mapped);
            cached_images.push((generation.image, generation.image_identity));
            if cached_images.len() > 2 {
                cached_images.remove(0);
            }
            images
                .prune(&generation.target, cached_images.iter().copied())
                .expect("retain only current and previous full images");
            if revision == 1 {
                first_snapshot = Some((
                    committed.identity(),
                    branch.commit(),
                    generation,
                    image_bytes,
                ));
            } else if revision == 3 {
                third_snapshot = Some((branch.commit(), generation.image_identity));
            }
        }

        let (first_generation_id, first_commit, first_generation, first_image) =
            first_snapshot.expect("first generation snapshot");
        let (third_commit, _third_identity) = third_snapshot.expect("third generation snapshot");
        assert_ne!(first_commit, third_commit);
        let mut history_gc = range_store
            .advance_history_gc(&first_generation.target)
            .expect("start metadata retention with the old tag present");
        while !history_gc.complete() {
            assert!(history_gc.processed_records() <= super::history::MAX_HISTORY_GC_BATCH_RECORDS);
            history_gc = range_store
                .advance_history_gc(&first_generation.target)
                .expect("continue metadata retention for the old tag");
        }
        assert!(history_gc.stats().live_commits() >= 3);
        assert!(
            images
                .open_identity(
                    &first_generation.target,
                    first_generation.image_identity,
                    first_generation.image.semantic_generation(),
                )
                .is_err()
        );
        drop(images);
        drop(generations);
        drop(range_store);
        drop(file_store);

        let reopened_store =
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("cold-open FileStore");
        let reopened = FileSemanticRangeStore::open(reopened_store, limits)
            .expect("cold-open semantic history adapter");
        reopened
            .collect_garbage_with_history(
                &first_generation.target,
                backend_store::GcLimits::default(),
            )
            .expect("force GC with tag and branch closure roots");
        drop(reopened);

        let reopened = FileSemanticRangeStore::open(
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("reopen FileStore after forced GC"),
            limits,
        )
        .expect("cold-reopen semantic history adapter after forced GC");
        let old_tag = HistoryRefName::new("old-v1").expect("historical tag");
        let reader = match reopened
            .checkout_history_image(&first_generation.target, HistoryRefKind::Tag, &old_tag)
            .expect("checkout historical full NXFI")
        {
            crate::HistoryImageCheckout::Resident(reader) => reader,
            crate::HistoryImageCheckout::NeedsHydration { .. } => {
                panic!("tagged V1 ordinal image should remain resident after GC")
            }
        };
        assert_eq!(reader.commit(), first_commit);
        assert_eq!(reader.image_identity(), first_generation.image_identity);
        assert_eq!(
            reader.generation(),
            first_generation.image.semantic_generation()
        );
        assert_eq!(reader.view().canonical_entities().len(), 1);
        let stats = reader.io_stats();
        let expected_bytes = u64::try_from(first_image.len()).expect("image length fits");
        assert_eq!(stats.segment_count(), 2);
        assert_eq!(stats.segment_validation_bytes_read(), expected_bytes);
        assert_eq!(stats.segment_copy_bytes_read(), expected_bytes);
        assert_eq!(stats.scratch_bytes_written(), expected_bytes);
        assert_eq!(stats.image_bytes_read_for_validation(), expected_bytes);
        assert!(reader.gc_pin_held_for() >= Duration::ZERO);
        let pressure = reopened
            .advance_history_gc(&first_generation.target)
            .expect_err("historical image checkout pins bridge metadata during GC");
        assert!(
            pressure.contains("deferred"),
            "unexpected GC result: {pressure}"
        );
        drop(reader);
        assert!(
            reopened
                .advance_history_gc(&first_generation.target)
                .expect("GC resumes after image reader drops")
                .complete()
        );

        let first_segment = first_generation
            .manifest
            .plane(SemanticPlaneKind::Ir(SemanticIrPlane::Core))
            .expect("first core plane")
            .segments()[0]
            .id_claim();
        let bridge = LocalSemanticGenerationFiles::open(&state_root)
            .expect("open generation bridge files")
            .target_root(&first_generation.target)
            .join("history")
            .join("segment-map")
            .join(format!("{}.map", hex(first_segment.as_bytes())));
        fs::remove_file(&bridge).expect("remove one historical bridge to simulate hydration gap");
        assert!(matches!(
            reopened
                .checkout_history_image(
                    &first_generation.target,
                    HistoryRefKind::Tag,
                    &old_tag,
                )
                .expect("missing bridge is a typed hydration state"),
            crate::HistoryImageCheckout::NeedsHydration {
                commit,
                image,
                image_identity,
            } if commit == first_commit
                && image == first_generation.image
                && image_identity == first_generation.image_identity
        ));
        let generation = reopened
            .history_commit(&first_generation.target, first_commit)
            .expect("old commit metadata still available");
        assert_eq!(generation.generation(), first_generation_id);
    }

    #[test]
    fn ancestry_pages_continue_past_checkpoints_with_snapshot_oracle_parity() {
        let directory = TestDirectory::create();
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let mut oracle = Vec::new();
        let mut latest_target = None;
        for revision in 1..=35_u64 {
            let payload = format!("sparse-history-generation-{revision}");
            let generation = fixture(
                payload.as_bytes(),
                revision,
                u8::try_from(revision).expect("small history revision"),
            );
            oracle.push(generation.manifest.root());
            latest_target = Some(generation.target.clone());
            let _ = commit(&files, &generation, [generation.stamp, generation.stamp])
                .expect("append selected history snapshot");
        }
        let target = latest_target.expect("history target exists");
        let tip = files
            .history_ref(
                &target,
                HistoryRefKind::Branch,
                &HistoryRefName::new("local-cache").expect("local cache ref"),
            )
            .expect("read selected ref")
            .expect("selected ref exists")
            .commit();
        drop(files);

        let reopened = LocalSemanticGenerationFiles::open(&directory.0).expect("reopen store");
        reset_history_replay_load_counts();
        let mut page = reopened
            .replay_history(&target, tip)
            .expect("read first bounded history page");
        assert_eq!(page.entries().len(), MAX_HISTORY_REPLAY_COMMITS);
        assert!(page.includes_checkpoint());
        assert_eq!(
            history_replay_load_counts(),
            HistoryReplayLoadCounts {
                commit_decodes: MAX_HISTORY_REPLAY_COMMITS + 1,
                generation_decodes: MAX_HISTORY_REPLAY_COMMITS + 1,
                locator_decodes: 0,
            },
            "one bounded page decodes its 32 returned nodes and one validated cursor lookahead"
        );
        let mut reverse_pages = vec![
            page.entries()
                .iter()
                .map(|entry| entry.generation().manifest().root())
                .collect::<Vec<_>>(),
        ];
        while let Some(cursor) = page.next_cursor() {
            page = reopened
                .continue_history_replay(&target, cursor)
                .expect("continue bounded history page");
            reverse_pages.push(
                page.entries()
                    .iter()
                    .map(|entry| entry.generation().manifest().root())
                    .collect(),
            );
        }
        reverse_pages.reverse();
        let replayed = reverse_pages.into_iter().flatten().collect::<Vec<_>>();
        assert_eq!(replayed, oracle);
    }

    #[test]
    fn history_pages_have_no_lifetime_commit_limit() {
        const HISTORY_LENGTH: u64 = 2_050;

        let directory = TestDirectory::create();
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let mut latest_target = None;
        for revision in 1..=HISTORY_LENGTH {
            let payload = format!("history-beyond-old-scan-cap-{revision}");
            let generation = fixture(
                payload.as_bytes(),
                revision,
                u8::try_from(revision % 251 + 1).expect("bounded fixture discriminator"),
            );
            latest_target = Some(generation.target.clone());
            let _ = commit(&files, &generation, [generation.stamp, generation.stamp])
                .expect("append long-lived history commit");
        }
        let target = latest_target.expect("history target exists");
        let tip = files
            .history_ref(
                &target,
                HistoryRefKind::Branch,
                &HistoryRefName::new("local-cache").expect("local cache ref"),
            )
            .expect("read local cache ref")
            .expect("local cache ref exists")
            .commit();
        drop(files);

        let reopened = LocalSemanticGenerationFiles::open(&directory.0).expect("reopen store");
        reset_history_replay_load_counts();
        let mut page = reopened
            .replay_history(&target, tip)
            .expect("read long history page");
        let mut replayed = page.entries().len();
        while let Some(cursor) = page.next_cursor() {
            page = reopened
                .continue_history_replay(&target, cursor)
                .expect("continue long history page");
            replayed += page.entries().len();
        }
        assert_eq!(
            replayed,
            usize::try_from(HISTORY_LENGTH).expect("history length fits")
        );
        let expected_loads = usize::try_from(HISTORY_LENGTH).expect("history length fits")
            + usize::try_from(HISTORY_LENGTH - 1).expect("history edge count fits")
                / MAX_HISTORY_REPLAY_COMMITS;
        assert_eq!(
            history_replay_load_counts(),
            HistoryReplayLoadCounts {
                commit_decodes: expected_loads,
                generation_decodes: expected_loads,
                locator_decodes: 0,
            },
            "full replay loads each node once plus one lookahead for every page boundary"
        );
    }

    #[test]
    fn replay_page_fails_closed_when_its_immediate_parent_is_malformed() {
        let directory = TestDirectory::create();
        let base = fixture(b"replay missing parent base", 1, 81);
        let next = fixture(b"replay missing parent next", 2, 82);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let _base_generation =
            commit(&files, &base, [base.stamp, base.stamp]).expect("commit base generation");
        let base_commit = files
            .history_ref(
                &base.target,
                HistoryRefKind::Branch,
                &HistoryRefName::new("local-cache").expect("local-cache ref"),
            )
            .expect("read base ref")
            .expect("base ref exists")
            .commit();
        let _ = commit(&files, &next, [next.stamp, next.stamp]).expect("commit next generation");
        let tip = files
            .history_ref(
                &next.target,
                HistoryRefKind::Branch,
                &HistoryRefName::new("local-cache").expect("local-cache ref"),
            )
            .expect("read next ref")
            .expect("next ref exists")
            .commit();
        let target_root = files.target_root(&next.target);
        let parent_path = target_root
            .join("history")
            .join("commits")
            .join(format!("{}.commit", hex(base_commit.as_bytes())));
        fs::write(&parent_path, b"malformed commit record")
            .expect("corrupt immediate parent commit");

        let error = files
            .replay_history(&next.target, tip)
            .expect_err("a malformed lookahead parent rejects the page");
        assert!(error.contains("length is invalid"), "{error}");
    }

    #[test]
    fn replay_page_pins_gc_and_cursor_fails_after_ref_move_and_prune() {
        let directory = TestDirectory::create();
        let cas_root = directory.0.join("cas");
        let store = FileStore::open(&cas_root, 16 * 1024 * 1024).expect("open FileStore");
        let limits = TransportLimits {
            max_chunk: 16 * 1024,
            ..TransportLimits::default()
        };
        let range_store = FileSemanticRangeStore::open(store, limits).expect("open range store");
        let files = LocalSemanticGenerationFiles::open(&cas_root.join("semantic-hydration"))
            .expect("open history files");
        let generation = fixture(b"replay page lease", 1, 83);
        let _ = commit(&files, &generation, [generation.stamp, generation.stamp])
            .expect("commit initial generation");
        let local_cache = HistoryRefName::new("local-cache").expect("local-cache ref");
        let first = files
            .history_ref(&generation.target, HistoryRefKind::Branch, &local_cache)
            .expect("read initial ref")
            .expect("initial ref exists")
            .commit();
        let mut tip = first;
        for step in 1..=MAX_HISTORY_REPLAY_COMMITS + 2 {
            let mut provenance = [0_u8; 32];
            provenance[0] = u8::try_from(step).expect("small replay fixture step");
            tip = admit_history(&files, &generation, &[tip], provenance).identity();
        }
        range_store
            .compare_and_swap_history_ref(
                &generation.target,
                HistoryRefKind::Branch,
                local_cache.clone(),
                Some(first),
                Some(tip),
            )
            .expect("select long replay line");
        let unrelated = admit_history(&files, &generation, &[], [0xfe; 32]);

        let writer_store = FileSemanticRangeStore::open(
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("open writer FileStore"),
            limits,
        )
        .expect("open concurrent ref writer");
        let (start_writer_tx, start_writer_rx) = std::sync::mpsc::channel();
        let (writer_started_tx, writer_started_rx) = std::sync::mpsc::channel();
        let (writer_done_tx, writer_done_rx) = std::sync::mpsc::channel();
        let writer_target = generation.target.clone();
        let writer_local_cache = local_cache.clone();
        let unrelated_identity = unrelated.identity();
        let writer = std::thread::spawn(move || {
            start_writer_rx
                .recv()
                .expect("page releases concurrent writer");
            writer_started_tx
                .send(())
                .expect("signal concurrent ref update start");
            let result = writer_store
                .compare_and_swap_history_ref(
                    &writer_target,
                    HistoryRefKind::Branch,
                    writer_local_cache,
                    Some(tip),
                    Some(unrelated_identity),
                )
                .map(|_| ());
            writer_done_tx
                .send(result)
                .expect("report concurrent ref update result");
        });

        let mut gc_was_blocked = false;
        let mut writer_started = false;
        let page = range_store
            .replay_history_with_page_hook(&generation.target, tip, || {
                if !writer_started {
                    start_writer_tx
                        .send(())
                        .expect("start ref update during replay page");
                    writer_started_rx
                        .recv_timeout(Duration::from_secs(2))
                        .expect("concurrent ref update reached its commit call");
                    match writer_done_rx.recv_timeout(Duration::from_millis(50)) {
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            panic!("concurrent ref writer exited before reporting its result")
                        }
                        Ok(result) => {
                            panic!(
                                "ref update completed while replay held its page lease: {result:?}"
                            )
                        }
                    }
                    writer_started = true;
                }
                if !gc_was_blocked {
                    let error = range_store
                        .advance_history_gc(&generation.target)
                        .expect_err("page lease prevents a concurrent history sweep");
                    assert!(error.contains("deferred"), "{error}");
                    gc_was_blocked = true;
                }
                Ok(())
            })
            .expect("bounded page remains complete while GC and ref update wait");
        assert_eq!(page.entries().len(), MAX_HISTORY_REPLAY_COMMITS);
        assert!(gc_was_blocked);
        let cursor = page.next_cursor().expect("older page remains");
        drop(page);
        writer.join().expect("concurrent ref writer thread");
        writer_done_rx
            .recv()
            .expect("concurrent ref writer reports result")
            .expect("ref moves after the page lease drops");

        let mut progress = range_store
            .advance_history_gc(&generation.target)
            .expect("collect after the ref moved");
        while !progress.complete() {
            progress = range_store
                .advance_history_gc(&generation.target)
                .expect("finish pruning the now-unrooted ancestry");
        }
        let error = range_store
            .continue_history_replay(&generation.target, cursor)
            .expect_err("the pruned cursor must fail closed without a partial page");
        assert!(error.contains("missing commit object"), "{error}");
    }

    #[test]
    fn torn_refs_missing_parent_or_generation_fail_before_recovery_prunes_records() {
        let directory = TestDirectory::create();
        let base = fixture(b"corruption base", 1, 61);
        let next = fixture(b"corruption next", 2, 62);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let base_head = commit(&files, &base, [base.stamp, base.stamp]).expect("commit base");
        let selected_name = HistoryRefName::new("local-cache").expect("local cache ref");
        let base_history_id = files
            .history_ref(&base.target, HistoryRefKind::Branch, &selected_name)
            .expect("read base history ref")
            .expect("base history ref exists")
            .commit();
        let next_head = commit(&files, &next, [next.stamp, next.stamp]).expect("commit next");
        let target_root = files.target_root(&next.target);
        let next_record = record_path(&target_root, next_head.identity());
        let base_record = record_path(&target_root, base_head.identity());
        let next_commit_path = target_root
            .join("history")
            .join("commits")
            .join(format!("{}.commit", hex(next_head.identity().as_bytes())));
        let base_commit_path = target_root
            .join("history")
            .join("commits")
            .join(format!("{}.commit", hex(base_history_id.as_bytes())));

        let refs_path = target_root.join("history").join("refs.catalog");
        let refs = fs::read(&refs_path).expect("read refs catalog");
        fs::write(&refs_path, b"torn refs catalog").expect("truncate refs catalog");
        assert!(
            files
                .current(&next.target)
                .expect_err("torn refs fail closed")
                .contains("length is invalid")
        );
        assert!(
            next_record.exists(),
            "no generation was pruned after torn refs"
        );
        fs::write(&refs_path, refs).expect("restore exact refs catalog");

        fs::remove_file(&base_commit_path).expect("remove a referenced parent commit");
        assert!(
            files
                .current(&next.target)
                .expect_err("missing parent fails closed")
                .contains("missing commit object")
        );
        assert!(
            next_commit_path.exists(),
            "child commit is retained on failure"
        );
        assert!(base_record.exists(), "generation records remain on failure");

        // Recreate the parent from the committed selected branch in a fresh
        // fixture store, then remove its generation snapshot independently.
        let recovered = TestDirectory::create();
        let recovered_files =
            LocalSemanticGenerationFiles::open(&recovered.0).expect("open second store");
        let base_head =
            commit(&recovered_files, &base, [base.stamp, base.stamp]).expect("commit base again");
        let next_head =
            commit(&recovered_files, &next, [next.stamp, next.stamp]).expect("commit next again");
        let target_root = recovered_files.target_root(&next.target);
        let missing_generation = record_path(&target_root, base_head.identity());
        fs::remove_file(missing_generation).expect("remove a referenced generation snapshot");
        assert!(
            recovered_files
                .current(&next.target)
                .expect_err("missing generation fails closed")
                .contains("missing record")
        );
        assert!(record_path(&target_root, next_head.identity()).exists());
    }

    #[test]
    fn deleting_last_branch_ref_reclaims_its_unreachable_commit_after_reopen() {
        let directory = TestDirectory::create();
        let base = fixture(b"branch retention base", 1, 71);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let _ = commit(&files, &base, [base.stamp, base.stamp]).expect("commit selected base");
        let scratch = admit_history(&files, &base, &[], [0x44; 32]);
        set_history_ref(
            &files,
            &base.target,
            HistoryRefKind::Branch,
            "scratch",
            None,
            Some(scratch.identity()),
        )
        .expect("create scratch branch");
        let scratch_path = files
            .target_root(&base.target)
            .join("history")
            .join("commits")
            .join(format!("{}.commit", hex(scratch.identity().as_bytes())));
        let scratch_bytes = fs::metadata(&scratch_path)
            .expect("read scratch commit size")
            .len();
        set_history_ref(
            &files,
            &base.target,
            HistoryRefKind::Branch,
            "scratch",
            Some(scratch.identity()),
            None,
        )
        .expect("delete scratch branch by CAS");
        drop(files);

        let after_ref_delete =
            LocalSemanticGenerationFiles::open(&directory.0).expect("reopen store");
        let _ = after_ref_delete
            .current(&base.target)
            .expect("recover retained selected branch")
            .expect("selected local head remains");
        arm_history_test_fault(HistoryTestFault::AfterHistoryIndexCompactRename);
        let interruption = loop {
            match after_ref_delete.advance_history_gc(&base.target) {
                Ok(progress) if progress.complete() => {
                    panic!("the injected compact-index interruption did not fire")
                }
                Ok(_) => {}
                Err(error) if error.contains("AfterHistoryIndexCompactRename") => break error,
                Err(error) => panic!("unexpected retention error: {error}"),
            }
        };
        assert!(interruption.contains("injected semantic history interruption"));
        assert!(
            !scratch_path.exists(),
            "commit unlink precedes index publication"
        );
        drop(after_ref_delete);

        let reopened = LocalSemanticGenerationFiles::open(&directory.0).expect("cold reopen");
        let mut progress = reopened
            .advance_history_gc(&base.target)
            .expect("recover compact-index rename");
        while !progress.complete() {
            assert!(
                progress.processed_records() <= super::history::MAX_HISTORY_GC_BATCH_RECORDS,
                "one retention cycle stays within its durable work budget"
            );
            progress = reopened
                .advance_history_gc(&base.target)
                .expect("continue bounded history retention");
        }
        assert!(progress.processed_records() <= super::history::MAX_HISTORY_GC_BATCH_RECORDS);
        assert!(!scratch_path.exists());
        assert_eq!(progress.stats().reclaimed_commits(), 1);
        assert_eq!(progress.stats().reclaimed_commit_bytes(), scratch_bytes);

        let history_root = reopened.target_root(&base.target).join("history");
        assert_eq!(
            fs::read_dir(history_root.join("gc"))
                .expect("enumerate bounded GC work roots")
                .count(),
            2,
            "only current reachability and retention work roots remain"
        );
        assert_eq!(
            fs::metadata(history_root.join("commit.index"))
                .expect("read compacted commit index")
                .len(),
            64,
            "only selected history ancestry remains indexed"
        );
        assert_eq!(
            fs::metadata(history_root.join("tombstones.index"))
                .expect("read compacted tombstone index")
                .len(),
            0,
            "consumed tombstones are compacted after retention"
        );
        drop(reopened);
        let cold = LocalSemanticGenerationFiles::open(&directory.0).expect("cold reopen");
        assert!(
            cold.history_ref(
                &base.target,
                HistoryRefKind::Branch,
                &HistoryRefName::new("local-cache").expect("local cache ref"),
            )
            .expect("read retained local branch after reopen")
            .is_some()
        );
    }

    #[test]
    fn map_unlink_crash_reopens_and_reconciles_capacity_before_compaction() {
        let directory = TestDirectory::create();
        let base = fixture(b"retention map crash", 1, 79);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let _ = commit(&files, &base, [base.stamp, base.stamp]).expect("commit selected base");
        let orphan_segment = backend_semantic::ir::UntrustedSemanticSegmentId::from_raw([0xa7; 32]);
        files
            .persist_history_segment_mapping(
                &base.target,
                orphan_segment,
                persisted_segment_object_id(&directory, b"orphan map one"),
                23,
            )
            .expect("persist known unreferenced bridge map");
        let target_root = files.target_root(&base.target);
        let history_root = target_root.join("history");
        let map_path = history_root
            .join("segment-map")
            .join(format!("{}.map", hex(orphan_segment.as_bytes())));
        assert!(map_path.exists());
        let map_file_bytes = fs::metadata(&map_path)
            .expect("read persisted map file size")
            .len();

        arm_history_test_fault(HistoryTestFault::AfterHistoryMapUnlink);
        let interruption = loop {
            match files.advance_history_gc(&base.target) {
                Ok(progress) if progress.complete() => {
                    panic!("the injected map-unlink interruption did not fire")
                }
                Ok(_) => {}
                Err(error) if error.contains("AfterHistoryMapUnlink") => break error,
                Err(error) => panic!("unexpected retention error: {error}"),
            }
        };
        assert!(interruption.contains("injected semantic history interruption"));
        assert!(!map_path.exists(), "unlink is durable before count repair");
        assert_eq!(
            history::decode_history_segment_map_count(
                &fs::read(history_root.join("segment-map.count")).expect("read old count")
            )
            .expect("decode old count"),
            1
        );
        drop(files);

        let reopened = LocalSemanticGenerationFiles::open(&directory.0).expect("cold reopen");
        let mut progress = reopened
            .advance_history_gc(&base.target)
            .expect("recover deletion intent after reopen");
        while !progress.complete() {
            assert!(progress.processed_records() <= super::history::MAX_HISTORY_GC_BATCH_RECORDS);
            progress = reopened
                .advance_history_gc(&base.target)
                .expect("continue recovered retention sweep");
        }
        assert_eq!(
            history::decode_history_segment_map_count(
                &fs::read(history_root.join("segment-map.count")).expect("read repaired count")
            )
            .expect("decode repaired count"),
            0
        );
        assert_eq!(progress.stats().reclaimed_maps(), 1);
        assert_eq!(progress.stats().reclaimed_map_bytes(), map_file_bytes);
        assert!(!map_path.exists());

        let next_segment = backend_semantic::ir::UntrustedSemanticSegmentId::from_raw([0xa8; 32]);
        reopened
            .persist_history_segment_mapping(
                &base.target,
                next_segment,
                persisted_segment_object_id(&directory, b"orphan map two"),
                29,
            )
            .expect("reclaimed map slot is reusable");
        assert_eq!(
            history::decode_history_segment_map_count(
                &fs::read(history_root.join("segment-map.count")).expect("read reused count")
            )
            .expect("decode reused count"),
            1
        );
    }

    #[test]
    fn corrupt_unreferenced_bridge_map_fails_closed_before_sweep() {
        let directory = TestDirectory::create();
        let base = fixture(b"corrupt history map", 1, 80);
        let files = LocalSemanticGenerationFiles::open(&directory.0).expect("open store");
        let _ = commit(&files, &base, [base.stamp, base.stamp]).expect("commit selected base");
        let orphan_segment = backend_semantic::ir::UntrustedSemanticSegmentId::from_raw([0xb7; 32]);
        files
            .persist_history_segment_mapping(
                &base.target,
                orphan_segment,
                persisted_segment_object_id(&directory, b"corrupt map fixture"),
                31,
            )
            .expect("persist known unreferenced bridge map");
        let map_path = files
            .target_root(&base.target)
            .join("history")
            .join("segment-map")
            .join(format!("{}.map", hex(orphan_segment.as_bytes())));
        fs::write(&map_path, b"corrupt bridge mapping").expect("corrupt exact map fixture");

        let error = (0..64)
            .find_map(|_| match files.advance_history_gc(&base.target) {
                Ok(progress) if progress.complete() => {
                    panic!("corrupt bridge map was swept instead of rejected")
                }
                Ok(_) => None,
                Err(error) => Some(error),
            })
            .expect("bounded retention reaches the corrupt bridge map");
        assert!(
            error.contains("checksum failed"),
            "unexpected error: {error}"
        );
        assert!(
            map_path.exists(),
            "fail-closed retention leaves corruption intact"
        );
    }
}
