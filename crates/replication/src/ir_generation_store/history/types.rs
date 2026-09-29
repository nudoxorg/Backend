// Included by the history facade to keep protocol internals in one private namespace.
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
pub(super) const HISTORY_SEGMENT_MAP_EPOCH_TAG: u8 = 12;
pub(super) const HISTORY_COMMIT_EPOCH_TAG: u8 = 13;
const MAX_HISTORY_PARENTS: usize = 2;
const MAX_HISTORY_REFS: usize = 512;
const MAX_REPLAY_COMMITS: usize = 32;
pub(super) const MAX_HISTORY_GC_BATCH_RECORDS: usize = 32;
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

/// Rejection from proposing history whose advertised payload closure cannot
/// be made complete by the current V1 publisher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryProposalError {
    /// The current publisher only carries the first-parent closure forward;
    /// it cannot safely advertise a merge with second-parent-only segments.
    UnsupportedMergePayloadClosure,
    /// A storage, authority, or validation failure while building the proposal.
    Storage(String),
}

impl std::fmt::Display for HistoryProposalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedMergePayloadClosure => formatter.write_str(
                "two-parent semantic history publication is unsupported until payload closures are unioned",
            ),
            Self::Storage(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for HistoryProposalError {}

impl From<String> for HistoryProposalError {
    fn from(message: String) -> Self {
        Self::Storage(message)
    }
}

/// A closure of already admitted segment objects that is cumulative across a
/// first-parent history line. The closure contains no semantic rows; it is a
/// FileStore reachability root over existing immutable payload objects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HistoryPayloadRoot {
    /// Untrusted bytes loaded from a payload-root side record. FileStore must
    /// admit this claim before it becomes a GC root or is used to read payloads.
    pub(crate) closure: ArtifactClosureClaim,
}

/// Payload root produced by a checked FileStore closure receipt and safe to
/// publish in the immutable history side record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AdmittedHistoryPayloadRoot {
    pub(crate) closure: ClosureId,
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
    pub const fn as_bytes(&self) -> &[u8; 32] {
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

    /// Returns this commit's validated first-parent depth.
    #[must_use]
    pub(crate) const fn first_parent_depth(&self) -> u32 {
        self.record.first_parent_depth
    }
}

/// Opaque proof that one commit is reachable from the exact tip of a named
/// history ref by following first-parent links.
///
/// Create this once and reuse it for materialization or segment reads. The
/// proof is bound to its target, ref name and kind, tip, and requested commit.
/// It is valid only while the ref currently has that exact tip; retargeting
/// away and back to the same content-addressed tip preserves the proof's
/// meaning. It is process-local and must be regenerated after reopening the
/// store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryRefAncestryProof {
    target: SemanticTargetKey,
    kind: HistoryRefKind,
    name: HistoryRefName,
    tip: HistoryCommitId,
    ancestor: HistoryCommitId,
}

impl HistoryRefAncestryProof {
    pub(crate) fn from_validated_first_parent_chain(
        target: SemanticTargetKey,
        kind: HistoryRefKind,
        name: HistoryRefName,
        tip: HistoryCommitId,
        ancestor: HistoryCommitId,
    ) -> Self {
        Self {
            target,
            kind,
            name,
            tip,
            ancestor,
        }
    }

    pub(crate) fn matches(
        &self,
        target: &SemanticTargetKey,
        kind: HistoryRefKind,
        name: &HistoryRefName,
        ancestor: HistoryCommitId,
    ) -> bool {
        self.target == *target
            && self.kind == kind
            && self.name == *name
            && self.ancestor == ancestor
    }

    /// Returns the exact ref tip this proof was issued for.
    #[must_use]
    pub const fn ref_tip(&self) -> HistoryCommitId {
        self.tip
    }

    /// Returns the requested reachable commit this proof covers.
    #[must_use]
    pub const fn ancestor(&self) -> HistoryCommitId {
        self.ancestor
    }
}

/// Receipt for immutable commit admission. A retry with the same proposal is
/// idempotent and reports `created == false`. The high-level admission API
/// keeps a shared FileStore GC pin in the receipt until it is dropped, so the
/// admitted commit can be published by a later ref CAS without racing metadata
/// retention.
#[derive(Clone, Debug)]
pub struct HistoryAdmissionReceipt {
    commit: AdmittedHistoryCommit,
    created: bool,
    _gc_pin: Option<std::sync::Arc<backend_store::GcPinGuard>>,
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

    pub(crate) fn with_gc_pin(mut self, gc_pin: std::sync::Arc<backend_store::GcPinGuard>) -> Self {
        self._gc_pin = Some(gc_pin);
        self
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

pub(super) fn ref_order(
    left: &SelectedHistoryRef,
    right: &SelectedHistoryRef,
) -> std::cmp::Ordering {
    left.name
        .cmp(&right.name)
        .then_with(|| left.kind.cmp(&right.kind))
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
    stats: HistoryGcStats,
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

    /// Returns cumulative reachability and reclamation counters for this GC
    /// epoch. Counters survive bounded batches and cold reopen.
    #[must_use]
    pub const fn stats(self) -> HistoryGcStats {
        self.stats
    }
}

/// Durable counters reported by semantic history retention.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HistoryGcStats {
    live_maps: u64,
    reclaimed_maps: u64,
    live_map_bytes: u64,
    reclaimed_map_bytes: u64,
    live_commits: u64,
    reclaimed_commits: u64,
    live_commit_bytes: u64,
    reclaimed_commit_bytes: u64,
    live_generation_records: u64,
    reclaimed_generation_records: u64,
    live_generation_record_bytes: u64,
    reclaimed_generation_record_bytes: u64,
}

impl HistoryGcStats {
    /// Number of bridge maps retained by current refs and live ancestry.
    #[must_use]
    pub const fn live_maps(self) -> u64 {
        self.live_maps
    }

    /// Number of bridge maps removed by this retention epoch.
    #[must_use]
    pub const fn reclaimed_maps(self) -> u64 {
        self.reclaimed_maps
    }

    /// Bytes occupied by retained bridge maps.
    #[must_use]
    pub const fn live_map_bytes(self) -> u64 {
        self.live_map_bytes
    }

    /// Bytes reclaimed from bridge maps.
    #[must_use]
    pub const fn reclaimed_map_bytes(self) -> u64 {
        self.reclaimed_map_bytes
    }

    /// Number of commit records retained by refs and their ancestry.
    #[must_use]
    pub const fn live_commits(self) -> u64 {
        self.live_commits
    }

    /// Number of unreferenced commit records swept in this epoch.
    #[must_use]
    pub const fn reclaimed_commits(self) -> u64 {
        self.reclaimed_commits
    }

    /// Bytes occupied by retained commit records.
    #[must_use]
    pub const fn live_commit_bytes(self) -> u64 {
        self.live_commit_bytes
    }

    /// Bytes reclaimed from commit records.
    #[must_use]
    pub const fn reclaimed_commit_bytes(self) -> u64 {
        self.reclaimed_commit_bytes
    }

    /// Number of generation records retained by refs, ancestry, or local HEAD.
    #[must_use]
    pub const fn live_generation_records(self) -> u64 {
        self.live_generation_records
    }

    /// Number of unreferenced generation records swept in this epoch.
    #[must_use]
    pub const fn reclaimed_generation_records(self) -> u64 {
        self.reclaimed_generation_records
    }

    /// Bytes occupied by retained generation records.
    #[must_use]
    pub const fn live_generation_record_bytes(self) -> u64 {
        self.live_generation_record_bytes
    }

    /// Bytes reclaimed from generation records.
    #[must_use]
    pub const fn reclaimed_generation_record_bytes(self) -> u64 {
        self.reclaimed_generation_record_bytes
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
