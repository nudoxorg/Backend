//! Persistent commit ancestry and named navigation references for admitted
//! semantic generations. Commits point at the existing immutable generation
//! records; they never copy or lower the canonical semantic IR.

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use backend_semantic::ir::{SemanticDeltaCursor, SemanticManifestError};
use backend_store::{ClosureId, ObjectId};

use super::{
    GenerationRecord, LocalSemanticGeneration, LocalSemanticGenerationFiles,
    LocalSemanticGenerationId, Reader, SelectedGenerationSource, SelectedGenerationStamp,
    SemanticTargetKey, Writer, checked_body, create_private_directory, display_io,
    ensure_directory, ensure_optional_directory, ensure_regular_file, hex, load_record,
    read_optional_bounded, remove_file, require_current, set_private_directory,
    validate_record_selection,
};

const HISTORY_COMMIT_TAG: u8 = 3;
const HISTORY_REFS_TAG: u8 = 4;
const HISTORY_GC_STATE_TAG: u8 = 5;
const HISTORY_COMMIT_DOMAIN: &[u8] = b"backend.semantic.history-commit.v1\0";
const HISTORY_PROVENANCE_DOMAIN: &[u8] = b"backend.semantic.history-admission-provenance.v1\0";
const HISTORY_INDEX_DOMAIN: &[u8] = b"backend.semantic.history-index.v1\0";
const HISTORY_TOMBSTONE_DOMAIN: &[u8] = b"backend.semantic.history-tombstone.v1\0";
const HISTORY_PAYLOAD_ROOT_TAG: u8 = 6;
const HISTORY_SEGMENT_MAP_TAG: u8 = 7;
const HISTORY_INDEX_INTENT_TAG: u8 = 8;
const HISTORY_SEGMENT_MAP_COUNT_TAG: u8 = 9;
const MAX_HISTORY_PARENTS: usize = 2;
const MAX_HISTORY_REFS: usize = 512;
const MAX_REPLAY_COMMITS: usize = 32;
const MAX_HISTORY_GC_BATCH_RECORDS: usize = 32;
const HISTORY_CHECKPOINT_INTERVAL: u32 = 16;
const MAX_HISTORY_REF_NAME_BYTES: usize = 255;
const MAX_HISTORY_COMMIT_BYTES: usize = 1_024;
const MAX_HISTORY_REFS_BYTES: usize = 1_048_576;
const HISTORY_INDEX_ENTRY_BYTES: u64 = 64;
const MAX_HISTORY_GC_STATE_BYTES: usize = 128;
const MAX_HISTORY_PAYLOAD_RECORD_BYTES: usize = 256;
const MAX_HISTORY_SEGMENT_MAP_BYTES: usize = 256;
const MAX_HISTORY_INDEX_INTENT_BYTES: usize = 128;
const MAX_HISTORY_SEGMENT_MAPPINGS: u32 = 65_536;
const MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES: usize = 64;

/// A closure of already admitted segment objects that is cumulative across a
/// first-parent history line. The closure contains no semantic rows; it is a
/// FileStore reachability root over existing immutable payload objects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct HistoryPayloadRoot {
    pub(super) closure: ClosureId,
}

/// Stable identity of a history commit. It is deliberately distinct from
/// [`GenerationId`](backend_semantic::ir::GenerationId): parentage and
/// admission provenance can distinguish commits with equal semantic roots.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HistoryCommitId([u8; 32]);

impl HistoryCommitId {
    /// Returns the content-derived commit identity bytes.
    #[must_use]
    pub const fn as_bytes(self) -> &[u8; 32] {
        &self.0
    }

    /// Wraps an untrusted fixed-width identifier claim. Every storage read or
    /// ref update validates that the named immutable record exists and hashes
    /// back to this identity before using it.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Versioned semantic-generation authority bound into a history commit ID.
///
/// V1 commits use the existing full-NXFI `GenerationId`. The V2 variant is
/// intentionally added only alongside the semantic crate's typed
/// `SemanticGenerationRootV2` verifier API; raw wire bytes cannot construct
/// that authority. The local generation-record ID remains a separate
/// materialization locator and is not this semantic root.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HistoryGenerationRoot {
    /// Existing NXFI-byte identity used by the V1 local history format.
    NxfiV1(backend_semantic::ir::GenerationId),
}

impl HistoryGenerationRoot {
    const fn wire_discriminator(self) -> u8 {
        match self {
            Self::NxfiV1(_) => 1,
        }
    }

    fn encode_root(self, writer: &mut Writer) -> Result<(), String> {
        match self {
            Self::NxfiV1(root) => {
                writer.u8(self.wire_discriminator())?;
                writer.fixed(root.as_bytes())
            }
        }
    }

    fn decode_root(reader: &mut Reader<'_>) -> Result<Self, String> {
        match reader.u8()? {
            1 => Ok(Self::NxfiV1(backend_semantic::ir::GenerationId::from_raw(
                reader.fixed()?,
            ))),
            2 => Err(
                "V2 history root requires the verified typed semantic-generation API".to_owned(),
            ),
            _ => Err("semantic history generation-root discriminator is invalid".to_owned()),
        }
    }
}

/// Whether a named reference is a movable branch or a named tag.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum HistoryRefKind {
    /// A reference intended to move as new history is admitted.
    Branch,
    /// A named historical pointer. Updates still require an explicit CAS.
    Tag,
}

impl HistoryRefKind {
    const fn wire(self) -> u8 {
        match self {
            Self::Branch => 1,
            Self::Tag => 2,
        }
    }

    fn from_wire(value: u8) -> Result<Self, String> {
        match value {
            1 => Ok(Self::Branch),
            2 => Ok(Self::Tag),
            _ => Err("semantic history reference has an invalid kind".to_owned()),
        }
    }
}

/// Validated display-independent name for one branch or tag.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HistoryRefName(String);

impl HistoryRefName {
    /// Creates a bounded path-like ref name with no empty or dot components.
    pub fn new(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        if value.is_empty()
            || value.len() > MAX_HISTORY_REF_NAME_BYTES
            || !value.is_ascii()
            || value.starts_with('/')
            || value.ends_with('/')
            || value.bytes().any(|byte| {
                !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'))
            })
            || value
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == "..")
        {
            return Err("semantic history reference name is invalid".to_owned());
        }
        Ok(Self(value))
    }

    /// Returns the validated reference name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A proposal for a commit whose semantic generation is already persisted,
/// but whose history identity has not yet been admitted.
#[derive(Clone, Debug)]
pub struct UnpublishedHistoryProposal {
    record: HistoryCommitRecord,
    identity: HistoryCommitId,
}

impl UnpublishedHistoryProposal {
    /// Returns the proposed history commit identity.
    #[must_use]
    pub const fn identity(&self) -> HistoryCommitId {
        self.identity
    }

    /// Returns the semantic generation that this proposal refers to.
    #[must_use]
    pub const fn generation(&self) -> LocalSemanticGenerationId {
        self.record.generation
    }

    /// Returns the versioned semantic-generation authority bound by this
    /// history identity, separate from its local materialization record.
    #[must_use]
    pub const fn generation_root(&self) -> HistoryGenerationRoot {
        self.record.generation_root
    }

    /// Returns the exact canonical semantic-plane manifest root.
    #[must_use]
    pub const fn manifest_root(&self) -> backend_semantic::ir::SemanticManifestRoot {
        self.record.manifest_root
    }
}

/// A commit record read back from durable storage after identity, authority
/// binding, canonical generation record, and ancestry checks succeeded.
#[derive(Clone, Debug)]
pub struct AdmittedHistoryCommit {
    record: HistoryCommitRecord,
}

impl AdmittedHistoryCommit {
    /// Returns this commit's history identity.
    #[must_use]
    pub const fn identity(&self) -> HistoryCommitId {
        self.record.identity
    }

    /// Returns the parent commit identities in first-parent order.
    #[must_use]
    pub fn parents(&self) -> &[HistoryCommitId] {
        &self.record.parents
    }

    /// Returns the referenced admitted semantic generation identity.
    #[must_use]
    pub const fn generation(&self) -> LocalSemanticGenerationId {
        self.record.generation
    }

    /// Returns the versioned semantic-generation authority bound by this
    /// history identity, separate from its local materialization record.
    #[must_use]
    pub const fn generation_root(&self) -> HistoryGenerationRoot {
        self.record.generation_root
    }

    /// Returns the exact canonical semantic-plane manifest root.
    #[must_use]
    pub const fn manifest_root(&self) -> backend_semantic::ir::SemanticManifestRoot {
        self.record.manifest_root
    }

    /// Returns the authority stamp saved when this history commit was
    /// admitted. Reopening it does not recreate a live authority capability.
    #[must_use]
    pub const fn selected_stamp(&self) -> SelectedGenerationStamp {
        self.record.stamp
    }

    /// Returns whether this commit is a bounded replay checkpoint.
    #[must_use]
    pub const fn is_checkpoint(&self) -> bool {
        self.record.checkpoint
    }
}

/// Receipt for immutable commit admission. A retry with the same proposal is
/// idempotent and reports `created == false`.
#[derive(Clone, Debug)]
pub struct HistoryAdmissionReceipt {
    commit: AdmittedHistoryCommit,
    created: bool,
}

impl HistoryAdmissionReceipt {
    /// Returns the admitted immutable commit.
    #[must_use]
    pub const fn commit(&self) -> &AdmittedHistoryCommit {
        &self.commit
    }

    /// Whether this call created the immutable commit file.
    #[must_use]
    pub const fn created(&self) -> bool {
        self.created
    }
}

/// One validated reference selected from the durable named-ref catalog.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedHistoryRef {
    kind: HistoryRefKind,
    name: HistoryRefName,
    commit: HistoryCommitId,
}

impl SelectedHistoryRef {
    /// Returns the branch or tag kind.
    #[must_use]
    pub const fn kind(&self) -> HistoryRefKind {
        self.kind
    }

    /// Returns the validated name.
    #[must_use]
    pub fn name(&self) -> &HistoryRefName {
        &self.name
    }

    /// Returns the history commit selected by this ref.
    #[must_use]
    pub const fn commit(&self) -> HistoryCommitId {
        self.commit
    }
}

/// Receipt for an atomic named-reference compare-and-swap or rename.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryRefUpdateReceipt {
    previous: Option<HistoryCommitId>,
    current: Option<HistoryCommitId>,
}

/// Bounded progress from one durable history mark/sweep batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HistoryGcProgress {
    processed_records: usize,
    complete: bool,
}

impl HistoryGcProgress {
    /// Returns the number of commit-index records processed by this batch.
    #[must_use]
    pub const fn processed_records(self) -> usize {
        self.processed_records
    }

    /// Whether this ref-catalog epoch completed its sweep.
    #[must_use]
    pub const fn complete(self) -> bool {
        self.complete
    }
}

impl HistoryRefUpdateReceipt {
    /// Returns the ref value observed by the successful CAS.
    #[must_use]
    pub const fn previous(&self) -> Option<HistoryCommitId> {
        self.previous
    }

    /// Returns the value made visible by the atomic catalog replacement.
    #[must_use]
    pub const fn current(&self) -> Option<HistoryCommitId> {
        self.current
    }
}

/// One replay step with its admitted commit and the existing canonical
/// generation record that the commit references.
#[derive(Debug)]
pub struct HistoryReplayEntry {
    commit: AdmittedHistoryCommit,
    generation: LocalSemanticGeneration,
}

impl HistoryReplayEntry {
    /// Returns the immutable history commit.
    #[must_use]
    pub const fn commit(&self) -> &AdmittedHistoryCommit {
        &self.commit
    }

    /// Returns the canonical generation snapshot admitted at commit time.
    #[must_use]
    pub const fn generation(&self) -> &LocalSemanticGeneration {
        &self.generation
    }
}

/// Availability of semantic bytes behind a durable history snapshot.
///
/// History commits retain canonical generation and manifest metadata, while
/// the bounded local image/segment cache may evict payload bytes. This type
/// prevents history retrieval from being mistaken for a readable IR image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryMaterialization {
    /// Every segment named by the commit's full typed-family manifest is
    /// present in the cumulative closure of the supplied named-ref tip and
    /// its segment-object bridge passed verification.
    ResidentSegments {
        /// Root selected by the named branch or tag used for this check.
        closure: Option<ClosureId>,
        /// Number of exact manifest segments verified in the closure.
        segment_count: usize,
        /// Total canonical payload bytes in those segments.
        byte_length: u64,
    },
    /// At least one manifest segment is not available in the named-ref
    /// payload closure. Rehydrate it under the existing authority before use.
    NeedsHydration {
        /// Exact canonical image key named by the history manifest.
        image: backend_semantic::ir::SemanticPlaneImageKey,
        /// Identity of the complete canonical image at admission time.
        image_identity: backend_semantic::ir::SemanticImageIdentity,
    },
}

/// Opaque continuation for bounded ancestry pages.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HistoryReplayCursor {
    next_ancestor: HistoryCommitId,
}

impl HistoryReplayCursor {
    /// Returns the next ancestor identity to read.
    #[must_use]
    pub const fn next_ancestor(self) -> HistoryCommitId {
        self.next_ancestor
    }
}

/// Bounded first-parent replay window ending at a requested commit. The first
/// entry is the nearest retained checkpoint; later entries are full admitted
/// generation snapshots that can be compared with the existing segment
/// cursor.
#[derive(Debug)]
pub struct HistoryReplay {
    entries: Vec<HistoryReplayEntry>,
    next: Option<HistoryReplayCursor>,
}

impl HistoryReplay {
    /// Returns one bounded oldest-to-newest page of the ancestry walk.
    #[must_use]
    pub fn entries(&self) -> &[HistoryReplayEntry] {
        &self.entries
    }

    /// Returns a continuation when older ancestors remain. The continuation
    /// can be used after a ref moves while this immutable ancestry remains
    /// reachable from another named ref.
    #[must_use]
    pub const fn next_cursor(&self) -> Option<HistoryReplayCursor> {
        self.next
    }

    /// Whether this page includes a checkpoint commit.
    #[must_use]
    pub fn includes_checkpoint(&self) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.commit.is_checkpoint())
    }

    /// Returns borrowed canonical manifest-segment deltas between adjacent
    /// replay snapshots. The semantic cursor keeps its exact-root, input,
    /// coverage, build-identity, and selected-head checks.
    #[must_use]
    pub fn segment_deltas(&self) -> HistorySegmentDeltas<'_> {
        HistorySegmentDeltas {
            entries: &self.entries,
            next: 0,
        }
    }
}

/// Borrowed segment-delta iterator over adjacent admitted replay snapshots.
pub struct HistorySegmentDeltas<'replay> {
    entries: &'replay [HistoryReplayEntry],
    next: usize,
}

impl<'replay> Iterator for HistorySegmentDeltas<'replay> {
    type Item = Result<SemanticDeltaCursor<'replay, 'replay>, SemanticManifestError>;

    fn next(&mut self) -> Option<Self::Item> {
        let before = self.entries.get(self.next)?;
        let after = self.entries.get(self.next.checked_add(1)?)?;
        self.next += 1;
        Some(
            before
                .generation
                .semantic_segment_delta(after.generation.manifest()),
        )
    }
}

/// Maximum number of commits returned by one first-parent ancestry page.
pub const MAX_HISTORY_REPLAY_COMMITS: usize = MAX_REPLAY_COMMITS;

#[derive(Clone, Debug, Eq, PartialEq)]
struct HistoryCommitRecord {
    identity: HistoryCommitId,
    target: SemanticTargetKey,
    parents: Vec<HistoryCommitId>,
    generation: LocalSemanticGenerationId,
    generation_root: HistoryGenerationRoot,
    manifest_root: backend_semantic::ir::SemanticManifestRoot,
    stamp: SelectedGenerationStamp,
    provenance: [u8; 32],
    first_parent_depth: u32,
    checkpoint: bool,
}

#[derive(Clone, Debug)]
struct HistoryRefCatalog {
    refs: Vec<SelectedHistoryRef>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryGcPhase {
    Mark,
    Sweep,
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HistoryGcState {
    refs_digest: [u8; 32],
    phase: HistoryGcPhase,
    tombstone_offset: u64,
    sweep_offset: u64,
}

impl HistoryRefCatalog {
    fn empty() -> Self {
        Self { refs: Vec::new() }
    }

    fn get(&self, kind: HistoryRefKind, name: &HistoryRefName) -> Option<HistoryCommitId> {
        self.refs
            .iter()
            .find(|reference| reference.kind == kind && reference.name == *name)
            .map(|reference| reference.commit)
    }

    fn set(&mut self, kind: HistoryRefKind, name: HistoryRefName, commit: Option<HistoryCommitId>) {
        self.refs
            .retain(|reference| reference.kind != kind || reference.name != name);
        if let Some(commit) = commit {
            self.refs.push(SelectedHistoryRef { kind, name, commit });
        }
        self.refs.sort_by(ref_order);
    }
}

fn ref_order(left: &SelectedHistoryRef, right: &SelectedHistoryRef) -> std::cmp::Ordering {
    left.name
        .cmp(&right.name)
        .then_with(|| left.kind.cmp(&right.kind))
}

fn read_history_catalog_snapshot(
    target_root: &Path,
) -> Result<(HistoryRefCatalog, [u8; 32]), String> {
    let history_root = target_root.join("history");
    if !ensure_optional_directory(&history_root)? {
        return Ok((HistoryRefCatalog::empty(), *blake3::hash(&[]).as_bytes()));
    }
    let catalog_path = history_root.join("refs.catalog");
    let bytes = read_optional_bounded(&catalog_path, MAX_HISTORY_REFS_BYTES)?
        .ok_or_else(|| "semantic history refs catalog is missing".to_owned())?;
    let digest = *blake3::hash(&bytes).as_bytes();
    Ok((decode_ref_catalog(&bytes)?, digest))
}

fn validate_history_commit_node(
    target_root: &Path,
    target: &SemanticTargetKey,
    commits_root: &Path,
    identity: HistoryCommitId,
) -> Result<HistoryCommitRecord, String> {
    let record = load_history_commit(commits_root, identity)?;
    if record.target != *target {
        return Err("semantic history commit belongs to another target".to_owned());
    }
    let generation = load_record(target_root, record.generation, target)?;
    validate_commit_generation(&record, &generation)?;
    validate_parent_set(commits_root, &record)?;
    for parent in &record.parents {
        let parent_record = load_history_commit(commits_root, *parent)?;
        let parent_generation = load_record(target_root, parent_record.generation, target)?;
        validate_commit_generation(&parent_record, &parent_generation)?;
    }
    Ok(record)
}

fn validate_catalog_tips(
    target_root: &Path,
    target: &SemanticTargetKey,
    catalog: &HistoryRefCatalog,
) -> Result<(), String> {
    let commits_root = target_root.join("history").join("commits");
    for reference in &catalog.refs {
        validate_history_commit_node(target_root, target, &commits_root, reference.commit)?;
    }
    Ok(())
}

/// History generation records are retained once the append-only index exists.
/// Metadata reclamation is intentionally coupled to history GC, never to a
/// current/previous cache open that cannot prove the full ref closure.
pub(super) fn may_prune_generation_records(
    target_root: &Path,
    _target: &SemanticTargetKey,
) -> Result<bool, String> {
    let history_root = target_root.join("history");
    if !ensure_optional_directory(&history_root)? {
        return Ok(true);
    }
    // `current()` and every commit consult this gate. Decode and checksum the
    // bounded ref catalog so corruption remains fail-closed, but do not open
    // every named tip on the hot path. Full tip/ancestry validation belongs
    // to ref reads, replay, and mark/sweep.
    let (catalog, _) = read_history_catalog_snapshot(target_root)?;
    let index_path = history_root.join("commit.index");
    let index_exists = match fs::symlink_metadata(&index_path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata.len() != 0,
        Ok(_) => return Err("semantic history index is not a regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(display_io(error)),
    };
    Ok(catalog.refs.is_empty() && !index_exists)
}

impl LocalSemanticGenerationFiles {
    pub(super) fn require_local_cache_head_alignment(
        &self,
        target: &SemanticTargetKey,
        cache_head: Option<super::LocalHead>,
        incoming_generation: LocalSemanticGenerationId,
        incoming_stamp: SelectedGenerationStamp,
        incoming_manifest_root: backend_semantic::ir::SemanticManifestRoot,
    ) -> Result<(), String> {
        let name = HistoryRefName::new("local-cache")?;
        let Some(reference) = self.history_ref(target, HistoryRefKind::Branch, &name)? else {
            return Ok(());
        };
        let selected = self.history_commit(target, reference.commit())?;
        let retries_durable_ref = selected.record.generation == incoming_generation
            && selected.record.stamp == incoming_stamp
            && selected.record.manifest_root == incoming_manifest_root;
        let follows_cache_head = cache_head.is_some_and(|head| {
            selected.record.generation == head.current && selected.record.stamp == head.stamp
        });
        if retries_durable_ref || follows_cache_head {
            Ok(())
        } else {
            Err(
                "local semantic history ref is ahead of or detached from cache HEAD; reconcile the ref before committing a new selected generation"
                    .to_owned(),
            )
        }
    }

    pub(crate) fn selected_history_payload_root(
        &self,
        target: &SemanticTargetKey,
    ) -> Result<Option<HistoryPayloadRoot>, String> {
        let name = HistoryRefName::new("local-cache")?;
        let Some(reference) = self.history_ref(target, HistoryRefKind::Branch, &name)? else {
            return Ok(None);
        };
        self.history_payload_root(target, reference.commit)
    }

    pub(crate) fn history_payload_root(
        &self,
        target: &SemanticTargetKey,
        commit: HistoryCommitId,
    ) -> Result<Option<HistoryPayloadRoot>, String> {
        let target_root = self.target_root(target);
        let _ = validate_history_commit_node(
            &target_root,
            target,
            &target_root.join("history").join("commits"),
            commit,
        )?;
        read_history_payload_root(&target_root, commit)
    }

    pub(crate) fn history_payload_roots(
        &self,
        target: &SemanticTargetKey,
    ) -> Result<Vec<ClosureId>, String> {
        let target_root = self.target_root(target);
        let history_root = target_root.join("history");
        if !ensure_optional_directory(&history_root)? {
            return Ok(Vec::new());
        }
        let (catalog, _) = read_history_catalog_snapshot(&target_root)?;
        validate_catalog_tips(&target_root, target, &catalog)?;
        let mut roots = Vec::with_capacity(catalog.refs.len());
        for reference in &catalog.refs {
            if let Some(payload) = read_history_payload_root(&target_root, reference.commit)? {
                roots.push(payload.closure);
            }
        }
        roots.sort_unstable();
        roots.dedup();
        Ok(roots)
    }

    pub(crate) fn persist_history_segment_mapping(
        &self,
        target: &SemanticTargetKey,
        segment: backend_semantic::ir::UntrustedSemanticSegmentId,
        object: ObjectId,
        byte_length: u64,
    ) -> Result<(), String> {
        let target_root = self.target_root(target);
        let history_root = target_root.join("history");
        prepare_history_layout(&target_root)?;
        let directory = history_root.join("segment-map");
        create_private_directory(&directory)?;
        set_private_directory(&directory)?;
        let bytes = encode_history_segment_mapping(segment, object, byte_length)?;
        let path = directory.join(format!("{}.map", hex(segment.as_bytes())));
        match fs::read(&path) {
            Ok(existing) if existing == bytes => Ok(()),
            Ok(_) => Err(
                "semantic history segment mapping conflicts with its content identity".to_owned(),
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                reserve_history_segment_mapping(&history_root, &directory)?;
                backend_platform::durable::write_private_atomic(&path, &bytes).map_err(display_io)
            }
            Err(error) => Err(display_io(error)),
        }
    }

    pub(crate) fn history_segment_mapping(
        &self,
        target: &SemanticTargetKey,
        segment: backend_semantic::ir::UntrustedSemanticSegmentId,
    ) -> Result<Option<(ObjectId, u64)>, String> {
        let path = self
            .target_root(target)
            .join("history")
            .join("segment-map")
            .join(format!("{}.map", hex(segment.as_bytes())));
        match read_optional_bounded(&path, MAX_HISTORY_SEGMENT_MAP_BYTES)? {
            Some(bytes) => decode_history_segment_mapping(&bytes, segment).map(Some),
            None => Ok(None),
        }
    }

    pub(crate) fn advance_history_gc(
        &self,
        target: &SemanticTargetKey,
    ) -> Result<HistoryGcProgress, String> {
        advance_history_gc(&self.target_root(target), target)
    }

    pub(crate) fn propose_history_commit(
        &self,
        target: &SemanticTargetKey,
        parents: &[HistoryCommitId],
        provenance: [u8; 32],
    ) -> Result<UnpublishedHistoryProposal, String> {
        if parents.len() > MAX_HISTORY_PARENTS {
            return Err("semantic history commit exceeds its parent bound".to_owned());
        }
        let generation = self
            .current(target)?
            .ok_or_else(|| "no admitted current generation is available for history".to_owned())?;
        let target_root = self.target_root(target);
        let commits_root = prepare_history_layout(&target_root)?;
        let mut depth = 0_u32;
        for (index, parent) in parents.iter().enumerate() {
            let record =
                validate_history_commit_node(&target_root, target, &commits_root, *parent)?;
            if index == 0 {
                depth = record
                    .first_parent_depth
                    .checked_add(1)
                    .ok_or_else(|| "semantic history depth overflows".to_owned())?;
            }
        }
        if parents.len() == 2 && parents[0] == parents[1] {
            return Err("semantic history commit repeats a parent".to_owned());
        }
        let record = HistoryCommitRecord {
            identity: HistoryCommitId([0; 32]),
            target: target.clone(),
            parents: parents.to_vec(),
            generation: generation.identity,
            generation_root: HistoryGenerationRoot::NxfiV1(generation.semantic_generation()),
            manifest_root: generation.manifest.root(),
            stamp: generation.selected_stamp,
            provenance,
            first_parent_depth: depth,
            checkpoint: parents.is_empty() || depth % HISTORY_CHECKPOINT_INTERVAL == 0,
        };
        let (record, identity) = identify_history_record(record)?;
        Ok(UnpublishedHistoryProposal { record, identity })
    }

    pub(crate) fn admit_history_proposal<S: SelectedGenerationSource>(
        &self,
        proposal: UnpublishedHistoryProposal,
        source: &mut S,
    ) -> Result<HistoryAdmissionReceipt, String> {
        let target_root = self.target_root(&proposal.record.target);
        let commits_root = prepare_history_layout(&target_root)?;
        let generation = load_record(
            &target_root,
            proposal.record.generation,
            &proposal.record.target,
        )?;
        validate_commit_generation(&proposal.record, &generation)?;
        require_current(source, proposal.record.stamp, generation.image)?;
        validate_parent_set(&commits_root, &proposal.record)?;
        let bytes = encode_history_commit(&proposal.record, proposal.identity)?;
        let path = history_commit_path(&commits_root, proposal.identity);
        let created = match fs::read(&path) {
            Ok(existing) if existing == bytes => false,
            Ok(_) => return Err("immutable semantic history identity collision".to_owned()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                backend_platform::durable::write_private_atomic(&path, &bytes)
                    .map_err(display_io)?;
                true
            }
            Err(error) => return Err(display_io(error)),
        };
        #[cfg(test)]
        if created {
            super::trip_history_test_fault(super::HistoryTestFault::AfterHistoryCommit)?;
        }
        let admitted = load_history_commit(&commits_root, proposal.identity)?;
        if admitted != proposal.record {
            return Err("admitted semantic history commit differs from its proposal".to_owned());
        }
        append_commit_index(&target_root, proposal.identity)?;
        Ok(HistoryAdmissionReceipt {
            commit: AdmittedHistoryCommit { record: admitted },
            created,
        })
    }

    pub(crate) fn history_ref(
        &self,
        target: &SemanticTargetKey,
        kind: HistoryRefKind,
        name: &HistoryRefName,
    ) -> Result<Option<SelectedHistoryRef>, String> {
        let target_root = self.target_root(target);
        if !ensure_optional_directory(&target_root.join("history"))? {
            return Ok(None);
        }
        let (catalog, _) = read_history_catalog_snapshot(&target_root)?;
        let Some(commit) = catalog.get(kind, name) else {
            return Ok(None);
        };
        let _ = validate_history_commit_node(
            &target_root,
            target,
            &target_root.join("history").join("commits"),
            commit,
        )?;
        Ok(Some(SelectedHistoryRef {
            kind,
            name: name.clone(),
            commit,
        }))
    }

    pub(crate) fn history_commit(
        &self,
        target: &SemanticTargetKey,
        identity: HistoryCommitId,
    ) -> Result<AdmittedHistoryCommit, String> {
        let target_root = self.target_root(target);
        let record = validate_history_commit_node(
            &target_root,
            target,
            &target_root.join("history").join("commits"),
            identity,
        )?;
        Ok(AdmittedHistoryCommit { record })
    }

    pub(crate) fn history_generation(
        &self,
        target: &SemanticTargetKey,
        identity: HistoryCommitId,
    ) -> Result<LocalSemanticGeneration, String> {
        let target_root = self.target_root(target);
        let record = validate_history_commit_node(
            &target_root,
            target,
            &target_root.join("history").join("commits"),
            identity,
        )?;
        let generation = load_record(&target_root, record.generation, target)?;
        generation_from_record(generation, record.stamp)
    }

    pub(crate) fn compare_and_swap_history_ref(
        &self,
        target: &SemanticTargetKey,
        kind: HistoryRefKind,
        name: HistoryRefName,
        expected: Option<HistoryCommitId>,
        next: Option<HistoryCommitId>,
    ) -> Result<HistoryRefUpdateReceipt, String> {
        let target_root = self.target_root(target);
        let commits_root = prepare_history_layout(&target_root)?;
        let (mut catalog, _) = read_history_catalog_snapshot(&target_root)?;
        validate_catalog_tips(&target_root, target, &catalog)?;
        let actual = catalog.get(kind, &name);
        if actual != expected {
            return Err("semantic history reference compare-and-swap failed".to_owned());
        }
        if let Some(identity) = next {
            let _ = validate_history_commit_node(&target_root, target, &commits_root, identity)?;
        }
        if let Some(previous) = actual {
            if Some(previous) != next {
                append_history_tombstone(&target_root, previous)?;
            }
        }
        catalog.set(kind, name, next);
        let encoded = encode_ref_catalog(&catalog)?;
        backend_platform::durable::write_private_atomic(
            &target_root.join("history").join("refs.catalog"),
            &encoded,
        )
        .map_err(display_io)?;
        #[cfg(test)]
        super::trip_history_test_fault(super::HistoryTestFault::AfterRefsCatalog)?;
        Ok(HistoryRefUpdateReceipt {
            previous: actual,
            current: next,
        })
    }

    pub(crate) fn rename_history_ref(
        &self,
        target: &SemanticTargetKey,
        kind: HistoryRefKind,
        old_name: &HistoryRefName,
        new_name: HistoryRefName,
        expected: HistoryCommitId,
    ) -> Result<HistoryRefUpdateReceipt, String> {
        let target_root = self.target_root(target);
        prepare_history_layout(&target_root)?;
        let (mut catalog, _) = read_history_catalog_snapshot(&target_root)?;
        validate_catalog_tips(&target_root, target, &catalog)?;
        let actual = catalog.get(kind, old_name);
        if actual != Some(expected) {
            return Err("semantic history reference compare-and-swap failed".to_owned());
        }
        if catalog.get(kind, &new_name).is_some() {
            return Err("semantic history rename destination already exists".to_owned());
        }
        catalog.set(kind, old_name.clone(), None);
        catalog.set(kind, new_name, Some(expected));
        let encoded = encode_ref_catalog(&catalog)?;
        backend_platform::durable::write_private_atomic(
            &target_root.join("history").join("refs.catalog"),
            &encoded,
        )
        .map_err(display_io)?;
        Ok(HistoryRefUpdateReceipt {
            previous: Some(expected),
            current: Some(expected),
        })
    }

    pub(crate) fn replay_history(
        &self,
        target: &SemanticTargetKey,
        tip: HistoryCommitId,
    ) -> Result<HistoryReplay, String> {
        self.replay_history_page(target, tip)
    }

    pub(crate) fn continue_history_replay(
        &self,
        target: &SemanticTargetKey,
        cursor: HistoryReplayCursor,
    ) -> Result<HistoryReplay, String> {
        self.replay_history_page(target, cursor.next_ancestor)
    }

    fn replay_history_page(
        &self,
        target: &SemanticTargetKey,
        start: HistoryCommitId,
    ) -> Result<HistoryReplay, String> {
        let target_root = self.target_root(target);
        let commits_root = target_root.join("history").join("commits");
        let mut entries = Vec::with_capacity(MAX_REPLAY_COMMITS);
        let mut cursor = start;
        let mut next = None;
        loop {
            let record =
                validate_history_commit_node(target_root.as_path(), target, &commits_root, cursor)?;
            let generation_record = load_record(target_root.as_path(), record.generation, target)?;
            let generation = generation_from_record(generation_record, record.stamp)?;
            let parent = record.parents.first().copied();
            entries.push(HistoryReplayEntry {
                commit: AdmittedHistoryCommit { record },
                generation,
            });
            let Some(parent) = parent else {
                break;
            };
            if entries.len() == MAX_REPLAY_COMMITS {
                next = Some(HistoryReplayCursor {
                    next_ancestor: parent,
                });
                break;
            }
            cursor = parent;
        }
        entries.reverse();
        Ok(HistoryReplay { entries, next })
    }

    pub(crate) fn record_selected_history<S: SelectedGenerationSource>(
        &self,
        generation: &LocalSemanticGeneration,
        source: &mut S,
        payload_root: Option<HistoryPayloadRoot>,
    ) -> Result<HistoryAdmissionReceipt, String> {
        let target = &generation.target;
        let target_root = self.target_root(target);
        prepare_history_layout(&target_root)?;
        let name = HistoryRefName::new("local-cache")?;
        let previous = self.history_ref(target, HistoryRefKind::Branch, &name)?;
        if let Some(reference) = &previous {
            let selected = self.history_commit(target, reference.commit)?;
            if selected.record.generation == generation.identity
                && selected.record.stamp == generation.selected_stamp
                && selected.record.manifest_root == generation.manifest.root()
            {
                require_current(source, generation.selected_stamp, generation.image)?;
                return Ok(HistoryAdmissionReceipt {
                    commit: selected,
                    created: false,
                });
            }
        }
        let parents = previous
            .iter()
            .map(SelectedHistoryRef::commit)
            .collect::<Vec<_>>();
        let provenance = selected_generation_provenance(generation);
        let first_parent_depth = if let Some(parent) = parents.first() {
            load_history_commit(&target_root.join("history").join("commits"), *parent)?
                .first_parent_depth
                .checked_add(1)
                .ok_or_else(|| "semantic history depth overflows".to_owned())?
        } else {
            0
        };
        let record = HistoryCommitRecord {
            identity: HistoryCommitId([0; 32]),
            target: target.clone(),
            parents: parents.clone(),
            generation: generation.identity,
            generation_root: HistoryGenerationRoot::NxfiV1(generation.semantic_generation()),
            manifest_root: generation.manifest.root(),
            stamp: generation.selected_stamp,
            provenance,
            first_parent_depth,
            checkpoint: parents.is_empty() || first_parent_depth % HISTORY_CHECKPOINT_INTERVAL == 0,
        };
        let (record, identity) = identify_history_record(record)?;
        let admission =
            self.admit_history_proposal(UnpublishedHistoryProposal { record, identity }, source)?;
        if let Some(payload_root) = payload_root {
            write_history_payload_root(&target_root, admission.commit.identity(), payload_root)?;
            #[cfg(test)]
            super::trip_history_test_fault(super::HistoryTestFault::AfterPayloadRoot)?;
        }
        self.compare_and_swap_selected_history_ref(
            target,
            previous.as_ref().map(SelectedHistoryRef::commit),
            admission.commit.identity(),
            source,
        )?;
        Ok(admission)
    }

    fn compare_and_swap_selected_history_ref<S: SelectedGenerationSource>(
        &self,
        target: &SemanticTargetKey,
        expected: Option<HistoryCommitId>,
        next: HistoryCommitId,
        source: &mut S,
    ) -> Result<HistoryRefUpdateReceipt, String> {
        let target_root = self.target_root(target);
        let commits_root = prepare_history_layout(&target_root)?;
        let record = load_history_commit(&commits_root, next)?;
        let generation = load_record(&target_root, record.generation, target)?;
        validate_commit_generation(&record, &generation)?;
        require_current(source, record.stamp, generation.image)?;
        self.compare_and_swap_history_ref(
            target,
            HistoryRefKind::Branch,
            HistoryRefName::new("local-cache")?,
            expected,
            Some(next),
        )
    }
}

fn generation_from_record(
    record: GenerationRecord,
    stamp: SelectedGenerationStamp,
) -> Result<LocalSemanticGeneration, String> {
    validate_record_selection(&record, stamp)?;
    Ok(LocalSemanticGeneration {
        identity: record.identity,
        previous_identity: None,
        previous_image: None,
        previous_image_identity: None,
        target: record.target,
        selected_stamp: stamp,
        catalog: record.catalog,
        image: record.image,
        image_identity: record.image_identity,
        manifest: record.manifest,
    })
}

fn validate_commit_generation(
    commit: &HistoryCommitRecord,
    generation: &GenerationRecord,
) -> Result<(), String> {
    validate_record_selection(generation, commit.stamp)?;
    if generation.identity != commit.generation
        || HistoryGenerationRoot::NxfiV1(generation.image.semantic_generation())
            != commit.generation_root
        || generation.manifest.root() != commit.manifest_root
    {
        return Err("semantic history commit differs from its generation snapshot".to_owned());
    }
    Ok(())
}

fn validate_commit_ancestry(
    record: &HistoryCommitRecord,
    records: &HashMap<HistoryCommitId, HistoryCommitRecord>,
) -> Result<(), String> {
    if record.parents.is_empty() {
        if record.first_parent_depth != 0 || !record.checkpoint {
            return Err("semantic history root has invalid checkpoint metadata".to_owned());
        }
        return Ok(());
    }
    let parent = records
        .get(&record.parents[0])
        .ok_or_else(|| "semantic history commit has a missing first parent".to_owned())?;
    if record.target != parent.target
        || record.first_parent_depth != parent.first_parent_depth.saturating_add(1)
        || record.checkpoint != (record.first_parent_depth % HISTORY_CHECKPOINT_INTERVAL == 0)
    {
        return Err("semantic history first-parent chain is inconsistent".to_owned());
    }
    for identity in record.parents.iter().skip(1) {
        let parent = records
            .get(identity)
            .ok_or_else(|| "semantic history commit has a missing merge parent".to_owned())?;
        if parent.target != record.target {
            return Err("semantic history merge crosses target scope".to_owned());
        }
    }
    Ok(())
}

fn validate_parent_set(commits_root: &Path, record: &HistoryCommitRecord) -> Result<(), String> {
    if record.parents.len() > MAX_HISTORY_PARENTS
        || record.parents.windows(2).any(|pair| pair[0] == pair[1])
    {
        return Err("semantic history commit has an invalid parent set".to_owned());
    }
    if record.parents.is_empty() {
        return validate_commit_ancestry(record, &HashMap::new());
    }
    let mut parents = HashMap::new();
    for identity in &record.parents {
        let parent = load_history_commit(commits_root, *identity)?;
        if parent.target != record.target {
            return Err("semantic history parent belongs to another target".to_owned());
        }
        parents.insert(*identity, parent);
    }
    validate_commit_ancestry(record, &parents)
}

fn identify_history_record(
    mut record: HistoryCommitRecord,
) -> Result<(HistoryCommitRecord, HistoryCommitId), String> {
    let body = encode_history_body(&record)?;
    let identity = history_commit_identity(&body);
    record.identity = identity;
    Ok((record, identity))
}

fn encode_history_body(record: &HistoryCommitRecord) -> Result<Vec<u8>, String> {
    let mut writer = Writer::new(MAX_HISTORY_COMMIT_BYTES - 32 - 32);
    writer.header(HISTORY_COMMIT_TAG)?;
    writer.target(&record.target)?;
    writer.u8(u8::try_from(record.parents.len())
        .map_err(|_| "semantic history parent count exceeds its bound".to_owned())?)?;
    for parent in &record.parents {
        writer.fixed(parent.as_bytes())?;
    }
    writer.fixed(&record.generation.0)?;
    record.generation_root.encode_root(&mut writer)?;
    writer.fixed(record.manifest_root.as_bytes())?;
    writer.stamp(record.stamp)?;
    writer.fixed(&record.provenance)?;
    writer.u32(record.first_parent_depth)?;
    writer.u8(u8::from(record.checkpoint))?;
    Ok(writer.finish())
}

fn encode_history_commit(
    record: &HistoryCommitRecord,
    identity: HistoryCommitId,
) -> Result<Vec<u8>, String> {
    let body = encode_history_body(record)?;
    if history_commit_identity(&body) != identity || record.identity != identity {
        return Err("semantic history proposal identity is inconsistent".to_owned());
    }
    let mut output = Vec::with_capacity(32 + body.len() + 32);
    output.extend_from_slice(&identity.0);
    output.extend_from_slice(&body);
    let checksum = blake3::hash(&output);
    output.extend_from_slice(checksum.as_bytes());
    if output.len() > MAX_HISTORY_COMMIT_BYTES {
        return Err("semantic history commit exceeds its storage bound".to_owned());
    }
    Ok(output)
}

fn decode_history_commit(bytes: &[u8]) -> Result<HistoryCommitRecord, String> {
    let body = checked_body(bytes, MAX_HISTORY_COMMIT_BYTES)?;
    if body.len() < 32 {
        return Err("semantic history commit is truncated".to_owned());
    }
    let identity = HistoryCommitId(
        body[..32]
            .try_into()
            .map_err(|_| "semantic history commit identity is truncated".to_owned())?,
    );
    let content = &body[32..];
    if history_commit_identity(content) != identity {
        return Err("semantic history commit identity does not match its record".to_owned());
    }
    let mut reader = Reader::new(content);
    reader.header(HISTORY_COMMIT_TAG)?;
    let target = reader.target()?;
    let parent_count = usize::from(reader.u8()?);
    if parent_count > MAX_HISTORY_PARENTS {
        return Err("semantic history commit exceeds its parent bound".to_owned());
    }
    let mut parents = Vec::with_capacity(parent_count);
    for _ in 0..parent_count {
        parents.push(HistoryCommitId(reader.fixed()?));
    }
    if parents.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("semantic history commit repeats a parent".to_owned());
    }
    let generation = LocalSemanticGenerationId(reader.fixed()?);
    let generation_root = HistoryGenerationRoot::decode_root(&mut reader)?;
    let manifest_root =
        backend_semantic::ir::SemanticManifestRoot::from_wire_claim(reader.fixed()?);
    let stamp = reader.stamp()?;
    let provenance = reader.fixed()?;
    let first_parent_depth = reader.u32()?;
    let checkpoint = match reader.u8()? {
        0 => false,
        1 => true,
        _ => return Err("semantic history checkpoint marker is invalid".to_owned()),
    };
    reader.finish()?;
    let record = HistoryCommitRecord {
        identity,
        target,
        parents,
        generation,
        generation_root,
        manifest_root,
        stamp,
        provenance,
        first_parent_depth,
        checkpoint,
    };
    if encode_history_body(&record)? != content {
        return Err("semantic history commit is not canonically encoded".to_owned());
    }
    Ok(record)
}

fn history_commit_identity(body: &[u8]) -> HistoryCommitId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(HISTORY_COMMIT_DOMAIN);
    hasher.update(&(body.len() as u64).to_be_bytes());
    hasher.update(body);
    HistoryCommitId(*hasher.finalize().as_bytes())
}

fn prepare_history_layout(target_root: &Path) -> Result<PathBuf, String> {
    create_private_directory(target_root)?;
    set_private_directory(target_root)?;
    backend_platform::durable::sync_parent(target_root).map_err(display_io)?;
    let history_root = target_root.join("history");
    let created = match fs::create_dir(&history_root) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure_directory(&history_root)?;
            false
        }
        Err(error) => return Err(display_io(error)),
    };
    set_private_directory(&history_root)?;
    if created {
        backend_platform::durable::sync_parent(&history_root).map_err(display_io)?;
    }
    let commits_root = history_root.join("commits");
    create_private_directory(&commits_root)?;
    set_private_directory(&commits_root)?;
    backend_platform::durable::sync_parent(&commits_root).map_err(display_io)?;
    for directory in ["indexed", "gc"] {
        let path = history_root.join(directory);
        create_private_directory(&path)?;
        set_private_directory(&path)?;
    }
    let refs_path = history_root.join("refs.catalog");
    if created {
        backend_platform::durable::write_private_atomic(
            &refs_path,
            &encode_ref_catalog(&HistoryRefCatalog::empty())?,
        )
        .map_err(display_io)?;
        backend_platform::durable::write_private_atomic(&history_root.join("commit.index"), &[])
            .map_err(display_io)?;
        backend_platform::durable::write_private_atomic(
            &history_root.join("tombstones.index"),
            &[],
        )
        .map_err(display_io)?;
    } else if read_optional_bounded(&refs_path, MAX_HISTORY_REFS_BYTES)?.is_none() {
        return Err("semantic history refs catalog is missing".to_owned());
    } else {
        let index_path = history_root.join("commit.index");
        if !index_path.exists() {
            return Err("semantic history commit index is missing".to_owned());
        }
        ensure_regular_file(&index_path)?;
        let tombstones_path = history_root.join("tombstones.index");
        if !tombstones_path.exists() {
            return Err("semantic history tombstone index is missing".to_owned());
        }
        ensure_regular_file(&tombstones_path)?;
    }
    Ok(commits_root)
}

fn encode_ref_catalog(catalog: &HistoryRefCatalog) -> Result<Vec<u8>, String> {
    if catalog.refs.len() > MAX_HISTORY_REFS
        || catalog
            .refs
            .windows(2)
            .any(|pair| ref_order(&pair[0], &pair[1]).is_ge())
    {
        return Err(
            "semantic history refs catalog is not canonical or exceeds its bound".to_owned(),
        );
    }
    let mut writer = Writer::new(MAX_HISTORY_REFS_BYTES - 32);
    writer.header(HISTORY_REFS_TAG)?;
    writer.u32(
        u32::try_from(catalog.refs.len())
            .map_err(|_| "semantic history ref count exceeds its bound".to_owned())?,
    )?;
    for reference in &catalog.refs {
        writer.u8(reference.kind.wire())?;
        writer.sized_bytes(
            reference.name.as_str().as_bytes(),
            MAX_HISTORY_REF_NAME_BYTES,
        )?;
        writer.fixed(reference.commit.as_bytes())?;
    }
    let mut bytes = writer.finish();
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    if bytes.len() > MAX_HISTORY_REFS_BYTES {
        return Err("semantic history refs catalog exceeds its storage bound".to_owned());
    }
    Ok(bytes)
}

fn decode_ref_catalog(bytes: &[u8]) -> Result<HistoryRefCatalog, String> {
    let body = checked_body(bytes, MAX_HISTORY_REFS_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HISTORY_REFS_TAG)?;
    let count = usize::try_from(reader.u32()?)
        .map_err(|_| "semantic history ref count exceeds address space".to_owned())?;
    if count > MAX_HISTORY_REFS {
        return Err("semantic history refs catalog exceeds its entry bound".to_owned());
    }
    let mut refs = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = HistoryRefKind::from_wire(reader.u8()?)?;
        let name = std::str::from_utf8(reader.sized_bytes(MAX_HISTORY_REF_NAME_BYTES)?)
            .map_err(|_| "semantic history ref name is not UTF-8".to_owned())?;
        let name = HistoryRefName::new(name)?;
        let commit = HistoryCommitId(reader.fixed()?);
        refs.push(SelectedHistoryRef { kind, name, commit });
    }
    reader.finish()?;
    if refs
        .windows(2)
        .any(|pair| ref_order(&pair[0], &pair[1]).is_ge())
    {
        return Err("semantic history refs catalog is unsorted or duplicated".to_owned());
    }
    Ok(HistoryRefCatalog { refs })
}

fn load_history_commit(
    commits_root: &Path,
    identity: HistoryCommitId,
) -> Result<HistoryCommitRecord, String> {
    let path = history_commit_path(commits_root, identity);
    let bytes = read_optional_bounded(&path, MAX_HISTORY_COMMIT_BYTES)?
        .ok_or_else(|| "semantic history references a missing commit object".to_owned())?;
    let record = decode_history_commit(&bytes)?;
    if record.identity != identity {
        return Err("semantic history commit filename differs from its identity".to_owned());
    }
    Ok(record)
}

fn history_commit_path(commits_root: &Path, identity: HistoryCommitId) -> PathBuf {
    commits_root.join(format!("{}.commit", hex(&identity.0)))
}

fn history_payload_root_path(target_root: &Path, identity: HistoryCommitId) -> PathBuf {
    target_root
        .join("history")
        .join("payload-roots")
        .join(format!("{}.root", hex(identity.as_bytes())))
}

fn write_history_payload_root(
    target_root: &Path,
    identity: HistoryCommitId,
    payload: HistoryPayloadRoot,
) -> Result<(), String> {
    let directory = target_root.join("history").join("payload-roots");
    create_private_directory(&directory)?;
    set_private_directory(&directory)?;
    let bytes = encode_history_payload_root(identity, payload)?;
    let path = history_payload_root_path(target_root, identity);
    match fs::read(&path) {
        Ok(existing) if existing == bytes => Ok(()),
        Ok(_) => Err("immutable semantic history payload root changed".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            backend_platform::durable::write_private_atomic(&path, &bytes).map_err(display_io)
        }
        Err(error) => Err(display_io(error)),
    }
}

fn read_history_payload_root(
    target_root: &Path,
    identity: HistoryCommitId,
) -> Result<Option<HistoryPayloadRoot>, String> {
    let path = history_payload_root_path(target_root, identity);
    let Some(bytes) = read_optional_bounded(&path, MAX_HISTORY_PAYLOAD_RECORD_BYTES)? else {
        return Ok(None);
    };
    decode_history_payload_root(&bytes, identity).map(Some)
}

fn encode_history_payload_root(
    identity: HistoryCommitId,
    payload: HistoryPayloadRoot,
) -> Result<Vec<u8>, String> {
    let mut writer = Writer::new(MAX_HISTORY_PAYLOAD_RECORD_BYTES - CHECKSUM_BYTES);
    writer.header(HISTORY_PAYLOAD_ROOT_TAG)?;
    writer.fixed(identity.as_bytes())?;
    writer.fixed(payload.closure.as_bytes())?;
    let mut bytes = writer.finish();
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    Ok(bytes)
}

fn decode_history_payload_root(
    bytes: &[u8],
    expected_identity: HistoryCommitId,
) -> Result<HistoryPayloadRoot, String> {
    let body = checked_body(bytes, MAX_HISTORY_PAYLOAD_RECORD_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HISTORY_PAYLOAD_ROOT_TAG)?;
    let identity = HistoryCommitId(reader.fixed()?);
    if identity != expected_identity {
        return Err("semantic history payload root names another commit".to_owned());
    }
    let closure = ClosureId::from_bytes(reader.fixed()?);
    reader.finish()?;
    Ok(HistoryPayloadRoot { closure })
}

fn reserve_history_segment_mapping(history_root: &Path, mapping_root: &Path) -> Result<(), String> {
    let count_path = history_root.join("segment-map.count");
    let count = match read_optional_bounded(&count_path, MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES)? {
        Some(bytes) => decode_history_segment_map_count(&bytes)?,
        None => {
            let mut count = 0_u32;
            for entry in fs::read_dir(mapping_root).map_err(display_io)? {
                let entry = entry.map_err(display_io)?;
                let path = entry.path();
                ensure_regular_file(&path)?;
                count = count
                    .checked_add(1)
                    .ok_or_else(|| "semantic history segment-map count overflows".to_owned())?;
                if count > MAX_HISTORY_SEGMENT_MAPPINGS {
                    return Err(
                        "semantic history segment-map retention limit was exceeded".to_owned()
                    );
                }
            }
            write_history_segment_map_count(&count_path, count)?;
            count
        }
    };
    let next = count
        .checked_add(1)
        .filter(|next| *next <= MAX_HISTORY_SEGMENT_MAPPINGS)
        .ok_or_else(|| {
            "semantic history segment-map retention limit reached; commit needs hydration or map compaction"
                .to_owned()
        })?;
    write_history_segment_map_count(&count_path, next)
}

fn write_history_segment_map_count(path: &Path, count: u32) -> Result<(), String> {
    let mut writer = Writer::new(MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES - CHECKSUM_BYTES);
    writer.header(HISTORY_SEGMENT_MAP_COUNT_TAG)?;
    writer.u32(count)?;
    let mut bytes = writer.finish();
    bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
    backend_platform::durable::write_private_atomic(path, &bytes).map_err(display_io)
}

fn decode_history_segment_map_count(bytes: &[u8]) -> Result<u32, String> {
    let body = checked_body(bytes, MAX_HISTORY_SEGMENT_MAP_COUNT_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HISTORY_SEGMENT_MAP_COUNT_TAG)?;
    let count = reader.u32()?;
    reader.finish()?;
    if count > MAX_HISTORY_SEGMENT_MAPPINGS {
        return Err("semantic history segment-map count exceeds its retention limit".to_owned());
    }
    Ok(count)
}

fn encode_history_segment_mapping(
    segment: backend_semantic::ir::UntrustedSemanticSegmentId,
    object: ObjectId,
    byte_length: u64,
) -> Result<Vec<u8>, String> {
    if byte_length == 0 {
        return Err("semantic history segment mapping has an empty payload".to_owned());
    }
    let mut writer = Writer::new(MAX_HISTORY_SEGMENT_MAP_BYTES - CHECKSUM_BYTES);
    writer.header(HISTORY_SEGMENT_MAP_TAG)?;
    writer.fixed(segment.as_bytes())?;
    writer.fixed(object.as_bytes())?;
    writer.u64(byte_length)?;
    let mut bytes = writer.finish();
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    Ok(bytes)
}

fn decode_history_segment_mapping(
    bytes: &[u8],
    expected_segment: backend_semantic::ir::UntrustedSemanticSegmentId,
) -> Result<(ObjectId, u64), String> {
    let body = checked_body(bytes, MAX_HISTORY_SEGMENT_MAP_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HISTORY_SEGMENT_MAP_TAG)?;
    let segment: [u8; 32] = reader.fixed()?;
    if segment != *expected_segment.as_bytes() {
        return Err("semantic history segment-map filename differs from its claim".to_owned());
    }
    let object = ObjectId::from_bytes(reader.fixed()?);
    let byte_length = reader.u64()?;
    if byte_length == 0 {
        return Err("semantic history segment mapping has an empty payload".to_owned());
    }
    reader.finish()?;
    Ok((object, byte_length))
}

fn append_commit_index(target_root: &Path, identity: HistoryCommitId) -> Result<(), String> {
    let history_root = target_root.join("history");
    recover_history_index_intent(target_root)?;
    let indexed_path = history_root
        .join("indexed")
        .join(format!("{}.indexed", hex(identity.as_bytes())));
    match fs::symlink_metadata(&indexed_path) {
        Ok(_) => {
            ensure_regular_file(&indexed_path)?;
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(display_io(error)),
    }
    let index_path = history_root.join("commit.index");
    ensure_regular_file(&index_path)?;
    let mut index = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&index_path)
        .map_err(display_io)?;
    let offset = repair_history_index_tail(&index_path, &mut index)?;
    let intent = HistoryIndexIntent { identity, offset };
    backend_platform::durable::write_private_atomic(
        &history_index_intent_path(target_root),
        &encode_history_index_intent(intent)?,
    )
    .map_err(display_io)?;
    append_history_index_entry(&index_path, identity, HISTORY_INDEX_DOMAIN)?;
    #[cfg(test)]
    super::trip_history_test_fault(super::HistoryTestFault::AfterHistoryIndex)?;
    backend_platform::durable::write_private_atomic(&indexed_path, &[]).map_err(display_io)?;
    remove_file(&history_index_intent_path(target_root))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HistoryIndexIntent {
    identity: HistoryCommitId,
    offset: u64,
}

fn history_index_intent_path(target_root: &Path) -> PathBuf {
    target_root.join("history").join("commit.index.intent")
}

fn encode_history_index_intent(intent: HistoryIndexIntent) -> Result<Vec<u8>, String> {
    let mut writer = Writer::new(MAX_HISTORY_INDEX_INTENT_BYTES - CHECKSUM_BYTES);
    writer.header(HISTORY_INDEX_INTENT_TAG)?;
    writer.fixed(intent.identity.as_bytes())?;
    writer.u64(intent.offset)?;
    let mut bytes = writer.finish();
    bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
    Ok(bytes)
}

fn decode_history_index_intent(bytes: &[u8]) -> Result<HistoryIndexIntent, String> {
    let body = checked_body(bytes, MAX_HISTORY_INDEX_INTENT_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HISTORY_INDEX_INTENT_TAG)?;
    let intent = HistoryIndexIntent {
        identity: HistoryCommitId(reader.fixed()?),
        offset: reader.u64()?,
    };
    reader.finish()?;
    if intent.offset % HISTORY_INDEX_ENTRY_BYTES != 0 {
        return Err("semantic history index intent has a misaligned offset".to_owned());
    }
    Ok(intent)
}

fn recover_history_index_intent(target_root: &Path) -> Result<(), String> {
    let intent_path = history_index_intent_path(target_root);
    let Some(bytes) = read_optional_bounded(&intent_path, MAX_HISTORY_INDEX_INTENT_BYTES)? else {
        return Ok(());
    };
    let intent = decode_history_index_intent(&bytes)?;
    let index_path = target_root.join("history").join("commit.index");
    ensure_regular_file(&index_path)?;
    let mut index = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&index_path)
        .map_err(display_io)?;
    let length = repair_history_index_tail(&index_path, &mut index)?;
    let expected_end = intent
        .offset
        .checked_add(HISTORY_INDEX_ENTRY_BYTES)
        .ok_or_else(|| "semantic history index intent overflows".to_owned())?;
    if length == intent.offset {
        append_history_index_entry(&index_path, intent.identity, HISTORY_INDEX_DOMAIN)?;
    } else if length == expected_end {
        let observed = history_index_id_at(&mut index, intent.offset, HISTORY_INDEX_DOMAIN)?;
        if observed != intent.identity {
            return Err(
                "semantic history index intent conflicts with its durable entry".to_owned(),
            );
        }
    } else {
        return Err("semantic history index intent is inconsistent with the append log".to_owned());
    }
    let indexed_path = target_root
        .join("history")
        .join("indexed")
        .join(format!("{}.indexed", hex(intent.identity.as_bytes())));
    match fs::symlink_metadata(&indexed_path) {
        Ok(_) => ensure_regular_file(&indexed_path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            backend_platform::durable::write_private_atomic(&indexed_path, &[])
                .map_err(display_io)?;
        }
        Err(error) => return Err(display_io(error)),
    }
    remove_file(&intent_path)
}

fn append_history_tombstone(target_root: &Path, identity: HistoryCommitId) -> Result<(), String> {
    append_history_index_entry(
        &target_root.join("history").join("tombstones.index"),
        identity,
        HISTORY_TOMBSTONE_DOMAIN,
    )
}

fn append_history_index_entry(
    index_path: &Path,
    identity: HistoryCommitId,
    domain: &[u8],
) -> Result<(), String> {
    ensure_regular_file(index_path)?;
    let mut index = OpenOptions::new()
        .read(true)
        .write(true)
        .append(true)
        .open(index_path)
        .map_err(display_io)?;
    let length = index.metadata().map_err(display_io)?.len();
    let tail = length % HISTORY_INDEX_ENTRY_BYTES;
    if tail != 0 {
        index.set_len(length - tail).map_err(display_io)?;
        index.sync_all().map_err(display_io)?;
    }
    let mut entry = Vec::with_capacity(HISTORY_INDEX_ENTRY_BYTES as usize);
    entry.extend_from_slice(identity.as_bytes());
    entry.extend_from_slice(&history_index_checksum(identity, domain));
    index.write_all(&entry).map_err(display_io)?;
    index.sync_all().map_err(display_io)
}

fn history_index_checksum(identity: HistoryCommitId, domain: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(identity.as_bytes());
    *hasher.finalize().as_bytes()
}

fn history_gc_state_path(target_root: &Path) -> PathBuf {
    target_root.join("history").join("gc.state")
}

fn encode_history_gc_state(state: HistoryGcState) -> Result<Vec<u8>, String> {
    let mut writer = Writer::new(MAX_HISTORY_GC_STATE_BYTES - 32);
    writer.header(HISTORY_GC_STATE_TAG)?;
    writer.fixed(&state.refs_digest)?;
    writer.u8(match state.phase {
        HistoryGcPhase::Mark => 1,
        HistoryGcPhase::Sweep => 2,
        HistoryGcPhase::Complete => 3,
    })?;
    writer.u64(state.tombstone_offset)?;
    writer.u64(state.sweep_offset)?;
    let mut bytes = writer.finish();
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    Ok(bytes)
}

fn decode_history_gc_state(bytes: &[u8]) -> Result<HistoryGcState, String> {
    let body = checked_body(bytes, MAX_HISTORY_GC_STATE_BYTES)?;
    let mut reader = Reader::new(body);
    reader.header(HISTORY_GC_STATE_TAG)?;
    let refs_digest = reader.fixed()?;
    let phase = match reader.u8()? {
        1 => HistoryGcPhase::Mark,
        2 => HistoryGcPhase::Sweep,
        3 => HistoryGcPhase::Complete,
        _ => return Err("semantic history GC phase is invalid".to_owned()),
    };
    let tombstone_offset = reader.u64()?;
    let sweep_offset = reader.u64()?;
    reader.finish()?;
    if tombstone_offset % HISTORY_INDEX_ENTRY_BYTES != 0
        || sweep_offset % HISTORY_INDEX_ENTRY_BYTES != 0
        || (phase == HistoryGcPhase::Mark && sweep_offset != 0)
    {
        return Err("semantic history GC cursor is invalid".to_owned());
    }
    Ok(HistoryGcState {
        refs_digest,
        phase,
        tombstone_offset,
        sweep_offset,
    })
}

fn read_history_gc_state(target_root: &Path) -> Result<Option<HistoryGcState>, String> {
    read_optional_bounded(
        &history_gc_state_path(target_root),
        MAX_HISTORY_GC_STATE_BYTES,
    )?
    .as_deref()
    .map(decode_history_gc_state)
    .transpose()
}

fn write_history_gc_state(target_root: &Path, state: HistoryGcState) -> Result<(), String> {
    let bytes = encode_history_gc_state(state)?;
    backend_platform::durable::write_private_atomic(&history_gc_state_path(target_root), &bytes)
        .map_err(display_io)
}

fn history_gc_epoch_root(target_root: &Path, digest: &[u8; 32]) -> PathBuf {
    target_root.join("history").join("gc").join(hex(digest))
}

fn ensure_history_gc_epoch(target_root: &Path, digest: &[u8; 32]) -> Result<PathBuf, String> {
    let epoch_root = history_gc_epoch_root(target_root, digest);
    create_private_directory(&epoch_root)?;
    for root in ["queue", "marks"] {
        let class_root = epoch_root.join(root);
        create_private_directory(&class_root)?;
        for class in ["live", "candidate"] {
            create_private_directory(&class_root.join(class))?;
        }
    }
    Ok(epoch_root)
}

fn marker_path(directory: &Path, identity: HistoryCommitId, suffix: &str) -> PathBuf {
    directory.join(format!("{}.{}", hex(identity.as_bytes()), suffix))
}

fn ensure_marker(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => ensure_regular_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            backend_platform::durable::write_private_atomic(path, &[]).map_err(display_io)
        }
        Err(error) => Err(display_io(error)),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryReachabilityClass {
    Live,
    Candidate,
}

impl HistoryReachabilityClass {
    const fn directory(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Candidate => "candidate",
        }
    }
}

fn history_gc_enqueue(
    epoch_root: &Path,
    identity: HistoryCommitId,
    class: HistoryReachabilityClass,
) -> Result<(), String> {
    ensure_marker(&marker_path(
        &epoch_root.join("queue").join(class.directory()),
        identity,
        "todo",
    ))
}

fn initialize_history_gc(
    target_root: &Path,
    catalog: &HistoryRefCatalog,
    digest: [u8; 32],
) -> Result<HistoryGcState, String> {
    let epoch_root = ensure_history_gc_epoch(target_root, &digest)?;
    for reference in &catalog.refs {
        history_gc_enqueue(
            &epoch_root,
            reference.commit,
            HistoryReachabilityClass::Live,
        )?;
    }
    let state = HistoryGcState {
        refs_digest: digest,
        phase: HistoryGcPhase::Mark,
        tombstone_offset: 0,
        sweep_offset: 0,
    };
    write_history_gc_state(target_root, state)?;
    Ok(state)
}

fn first_history_gc_todo(
    epoch_root: &Path,
) -> Result<Option<(PathBuf, HistoryCommitId, HistoryReachabilityClass)>, String> {
    for class in [
        HistoryReachabilityClass::Live,
        HistoryReachabilityClass::Candidate,
    ] {
        let queue = epoch_root.join("queue").join(class.directory());
        for entry in fs::read_dir(&queue).map_err(display_io)? {
            let entry = entry.map_err(display_io)?;
            let path = entry.path();
            ensure_regular_file(&path)?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "semantic history GC queue name is not UTF-8".to_owned())?;
            let stem = name
                .strip_suffix(".todo")
                .ok_or_else(|| "semantic history GC queue contains an unknown member".to_owned())?;
            if stem.len() != 64 {
                return Err("semantic history GC queue identity is malformed".to_owned());
            }
            return Ok(Some((
                path,
                HistoryCommitId(decode_hex_digest(stem)?),
                class,
            )));
        }
    }
    Ok(None)
}

fn history_gc_marked(
    epoch_root: &Path,
    identity: HistoryCommitId,
    class: HistoryReachabilityClass,
) -> Result<bool, String> {
    let path = marker_path(
        &epoch_root.join("marks").join(class.directory()),
        identity,
        "mark",
    );
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            ensure_regular_file(&path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(display_io(error)),
    }
}

fn repair_history_index_tail(index_path: &Path, index: &mut File) -> Result<u64, String> {
    ensure_regular_file(index_path)?;
    let length = index.metadata().map_err(display_io)?.len();
    let tail = length % HISTORY_INDEX_ENTRY_BYTES;
    if tail != 0 {
        index.set_len(length - tail).map_err(display_io)?;
        index.sync_all().map_err(display_io)?;
    }
    Ok(length - tail)
}

fn history_index_id_at(
    index: &mut File,
    offset: u64,
    domain: &[u8],
) -> Result<HistoryCommitId, String> {
    index.seek(SeekFrom::Start(offset)).map_err(display_io)?;
    let mut entry = [0; HISTORY_INDEX_ENTRY_BYTES as usize];
    index.read_exact(&mut entry).map_err(display_io)?;
    let identity = HistoryCommitId(
        entry[..32]
            .try_into()
            .map_err(|_| "semantic history index identity is truncated".to_owned())?,
    );
    if entry[32..] != history_index_checksum(identity, domain) {
        return Err("semantic history commit index checksum is invalid".to_owned());
    }
    Ok(identity)
}

fn advance_history_gc(
    target_root: &Path,
    target: &SemanticTargetKey,
) -> Result<HistoryGcProgress, String> {
    let history_root = target_root.join("history");
    if !ensure_optional_directory(&history_root)? {
        return Ok(HistoryGcProgress {
            processed_records: 0,
            complete: true,
        });
    }
    let (catalog, digest) = read_history_catalog_snapshot(target_root)?;
    validate_catalog_tips(target_root, target, &catalog)?;
    let state_path = history_gc_state_path(target_root);
    let mut state = match read_history_gc_state(target_root)? {
        Some(state) if state.refs_digest == digest => state,
        _ => initialize_history_gc(target_root, &catalog, digest)?,
    };
    let epoch_root = ensure_history_gc_epoch(target_root, &digest)?;
    let mut processed = 0;
    loop {
        match state.phase {
            HistoryGcPhase::Mark => {
                if let Some((todo_path, identity, class)) = first_history_gc_todo(&epoch_root)? {
                    if history_gc_marked(&epoch_root, identity, class)? {
                        remove_file(&todo_path)?;
                        processed += 1;
                    } else {
                        let commits_root = history_root.join("commits");
                        let record = validate_history_commit_node(
                            target_root,
                            target,
                            &commits_root,
                            identity,
                        )?;
                        for parent in &record.parents {
                            history_gc_enqueue(&epoch_root, *parent, class)?;
                        }
                        ensure_marker(&marker_path(
                            &epoch_root.join("marks").join(class.directory()),
                            identity,
                            "mark",
                        ))?;
                        remove_file(&todo_path)?;
                        processed += 1;
                    }
                } else {
                    let tombstones_path = history_root.join("tombstones.index");
                    ensure_regular_file(&tombstones_path)?;
                    let mut tombstones = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&tombstones_path)
                        .map_err(display_io)?;
                    let length = repair_history_index_tail(&tombstones_path, &mut tombstones)?;
                    if state.tombstone_offset > length {
                        return Err(
                            "semantic history GC cursor exceeds its tombstone index".to_owned()
                        );
                    }
                    if state.tombstone_offset < length {
                        let identity = history_index_id_at(
                            &mut tombstones,
                            state.tombstone_offset,
                            HISTORY_TOMBSTONE_DOMAIN,
                        )?;
                        history_gc_enqueue(
                            &epoch_root,
                            identity,
                            HistoryReachabilityClass::Candidate,
                        )?;
                        state.tombstone_offset = state
                            .tombstone_offset
                            .checked_add(HISTORY_INDEX_ENTRY_BYTES)
                            .ok_or_else(|| {
                                "semantic history GC tombstone cursor overflows".to_owned()
                            })?;
                        processed += 1;
                    } else {
                        state.phase = HistoryGcPhase::Sweep;
                        state.sweep_offset = 0;
                        write_history_gc_state(target_root, state)?;
                        continue;
                    }
                }
                if processed >= MAX_HISTORY_GC_BATCH_RECORDS {
                    write_history_gc_state(target_root, state)?;
                    return Ok(HistoryGcProgress {
                        processed_records: processed,
                        complete: false,
                    });
                }
            }
            HistoryGcPhase::Sweep => {
                let (current_catalog, current_digest) = read_history_catalog_snapshot(target_root)?;
                if current_digest != state.refs_digest {
                    let restarted =
                        initialize_history_gc(target_root, &current_catalog, current_digest)?;
                    return Ok(HistoryGcProgress {
                        processed_records: processed,
                        complete: restarted.phase == HistoryGcPhase::Complete,
                    });
                }
                validate_catalog_tips(target_root, target, &current_catalog)?;
                let tombstones_path = history_root.join("tombstones.index");
                ensure_regular_file(&tombstones_path)?;
                let mut tombstones = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&tombstones_path)
                    .map_err(display_io)?;
                let tombstones_length =
                    repair_history_index_tail(&tombstones_path, &mut tombstones)?;
                if state.tombstone_offset > tombstones_length {
                    return Err("semantic history GC cursor exceeds its tombstone index".to_owned());
                }
                if state.tombstone_offset < tombstones_length {
                    state.phase = HistoryGcPhase::Mark;
                    write_history_gc_state(target_root, state)?;
                    continue;
                }
                let index_path = history_root.join("commit.index");
                ensure_regular_file(&index_path)?;
                let mut index = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&index_path)
                    .map_err(display_io)?;
                let length = repair_history_index_tail(&index_path, &mut index)?;
                if state.sweep_offset > length {
                    return Err("semantic history GC cursor exceeds its commit index".to_owned());
                }
                if state.sweep_offset == length {
                    state.phase = HistoryGcPhase::Complete;
                    write_history_gc_state(target_root, state)?;
                    return Ok(HistoryGcProgress {
                        processed_records: processed,
                        complete: true,
                    });
                }
                while state.sweep_offset < length && processed < MAX_HISTORY_GC_BATCH_RECORDS {
                    let identity =
                        history_index_id_at(&mut index, state.sweep_offset, HISTORY_INDEX_DOMAIN)?;
                    if history_gc_marked(
                        &epoch_root,
                        identity,
                        HistoryReachabilityClass::Candidate,
                    )? && !history_gc_marked(
                        &epoch_root,
                        identity,
                        HistoryReachabilityClass::Live,
                    )? {
                        remove_file(&history_commit_path(
                            &history_root.join("commits"),
                            identity,
                        ))?;
                        remove_file(&history_payload_root_path(target_root, identity))?;
                    }
                    state.sweep_offset = state
                        .sweep_offset
                        .checked_add(HISTORY_INDEX_ENTRY_BYTES)
                        .ok_or_else(|| "semantic history GC cursor overflows".to_owned())?;
                    processed += 1;
                }
                if state.sweep_offset == length {
                    state.phase = HistoryGcPhase::Complete;
                }
                write_history_gc_state(target_root, state)?;
                return Ok(HistoryGcProgress {
                    processed_records: processed,
                    complete: state.phase == HistoryGcPhase::Complete,
                });
            }
            HistoryGcPhase::Complete => {
                let tombstones_path = history_root.join("tombstones.index");
                ensure_regular_file(&tombstones_path)?;
                let mut tombstones = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&tombstones_path)
                    .map_err(display_io)?;
                let tombstones_length =
                    repair_history_index_tail(&tombstones_path, &mut tombstones)?;
                if state.tombstone_offset < tombstones_length {
                    state.phase = HistoryGcPhase::Mark;
                    write_history_gc_state(target_root, state)?;
                    continue;
                }
                let index_path = history_root.join("commit.index");
                ensure_regular_file(&index_path)?;
                let mut index = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&index_path)
                    .map_err(display_io)?;
                let length = repair_history_index_tail(&index_path, &mut index)?;
                if state.sweep_offset < length {
                    state.phase = HistoryGcPhase::Sweep;
                    write_history_gc_state(target_root, state)?;
                    continue;
                }
                return Ok(HistoryGcProgress {
                    processed_records: processed,
                    complete: true,
                });
            }
        }
    }
}

fn decode_hex_digest(value: &str) -> Result<[u8; 32], String> {
    let mut output = [0; 32];
    for (index, bytes) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = (bytes[0] as char)
            .to_digit(16)
            .ok_or_else(|| "semantic history filename is malformed".to_owned())?;
        let low = (bytes[1] as char)
            .to_digit(16)
            .ok_or_else(|| "semantic history filename is malformed".to_owned())?;
        output[index] = u8::try_from((high << 4) | low)
            .map_err(|_| "semantic history filename is malformed".to_owned())?;
    }
    Ok(output)
}

fn selected_generation_provenance(generation: &LocalSemanticGeneration) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(HISTORY_PROVENANCE_DOMAIN);
    hasher.update(generation.selected_stamp.namespace());
    hasher.update(&<[u8; 2]>::from(generation.selected_stamp.profile()));
    hasher.update(generation.selected_stamp.source_coordinate());
    hasher.update(&generation.selected_stamp.selection_revision().to_be_bytes());
    hasher.update(generation.selected_stamp.selected_root());
    hasher.update(generation.selected_stamp.closure_id());
    hasher.update(generation.selected_stamp.catalog_root().as_bytes());
    hasher.update(generation.image_identity.as_ref());
    hasher.update(generation.manifest.root().as_bytes());
    *hasher.finalize().as_bytes()
}
