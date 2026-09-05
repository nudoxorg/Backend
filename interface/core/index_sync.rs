//! Typed local-first index synchronization vocabulary.
//!
//! This module owns the client control plane only. Immutable segment payloads remain in the
//! caller's store and are selected through borrowed descriptors and caller-owned scratch.

use core::num::NonZeroU64;

use arrayvec::ArrayVec;
use heart_identity::{
    ContentId, GenerationId, IndexExactSegmentDomain, IndexLexicalSegmentDomain, IndexSnapshotId,
    IndexSnapshotIdentityError, ObjectDomain, derive_index_snapshot,
};
use server_index_core::EntityDocumentId;

/// The maximum number of requested segment ranges in one local selection.
pub const MAX_CLIENT_DEMANDS: usize = 256;
/// Maximum immutable segment identities accepted in either canonical snapshot lane.
pub const MAX_CLIENT_SEGMENTS_PER_LANE: usize = 8;
/// Maximum independently tracked local code/environment overlay keys.
pub const MAX_CLIENT_OVERLAY_ENTRIES: usize = 256;
/// Maximum immutable local range receipts retained by the compact client control plane.
pub const MAX_CLIENT_RESIDENT_RANGES: usize = 256;
/// Maximum disposable local projection records retained by the compact client control plane.
pub const MAX_CLIENT_PROJECTIONS: usize = 8;

/// The accepted generation and snapshot used as a client's canonical base.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BaseGeneration {
    generation: GenerationId,
    snapshot: IndexSnapshotId,
}

impl BaseGeneration {
    /// Binds one immutable index snapshot to its compiler generation.
    #[must_use]
    pub const fn new(generation: GenerationId, snapshot: IndexSnapshotId) -> Self {
        Self {
            generation,
            snapshot,
        }
    }

    /// Returns the immutable compiler generation authority.
    #[must_use]
    pub const fn generation(self) -> GenerationId {
        self.generation
    }

    /// Returns the immutable index snapshot authority.
    #[must_use]
    pub const fn snapshot(self) -> IndexSnapshotId {
        self.snapshot
    }
}

/// A remote generation advertised before client acceptance.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RemoteGeneration {
    generation: GenerationId,
    snapshot: IndexSnapshotId,
}

impl RemoteGeneration {
    /// Binds a remote advertisement to one immutable compiler generation and snapshot.
    #[must_use]
    pub const fn new(generation: GenerationId, snapshot: IndexSnapshotId) -> Self {
        Self {
            generation,
            snapshot,
        }
    }

    /// Returns the advertised compiler generation authority.
    #[must_use]
    pub const fn generation(self) -> GenerationId {
        self.generation
    }

    /// Returns the advertised index snapshot authority.
    #[must_use]
    pub const fn snapshot(self) -> IndexSnapshotId {
        self.snapshot
    }

    const fn accepted(self) -> BaseGeneration {
        BaseGeneration::new(self.generation, self.snapshot)
    }

    fn matches(self, accepted: BaseGeneration) -> bool {
        self.generation == accepted.generation && self.snapshot == accepted.snapshot
    }
}

/// Monotonic sequence number for one remote manifest stream.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ManifestEpoch(NonZeroU64);

impl ManifestEpoch {
    /// Creates one nonzero remote manifest sequence number.
    #[must_use]
    pub const fn new(value: NonZeroU64) -> Self {
        Self(value)
    }

    /// Returns the sequence number for protocol serialization.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// Family-specific immutable segment identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SegmentId {
    /// Exact-key segment identity.
    Exact(ContentId<IndexExactSegmentDomain>),
    /// Lexical segment identity.
    Lexical(ContentId<IndexLexicalSegmentDomain>),
}

/// A nonempty checked half-open byte range within one immutable segment.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SegmentRange {
    start: u64,
    end: u64,
}

/// Exact reason a segment range was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentRangeError {
    /// The range endpoint overflowed the coordinate width.
    Overflow,
}

impl SegmentRange {
    /// Validates a nonempty half-open range from a start and byte length.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentRangeError::Overflow`] if the exclusive endpoint does not fit in `u64`.
    pub const fn new(start: u64, byte_length: NonZeroU64) -> Result<Self, SegmentRangeError> {
        let Some(end) = start.checked_add(byte_length.get()) else {
            return Err(SegmentRangeError::Overflow);
        };
        Ok(Self { start, end })
    }

    /// Returns the inclusive start coordinate.
    #[must_use]
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Returns the exclusive end coordinate.
    #[must_use]
    pub const fn end(self) -> u64 {
        self.end
    }

    /// Returns the exact byte length.
    #[must_use]
    pub const fn byte_length(self) -> u64 {
        self.end - self.start
    }

    const fn contains(self, requested: Self) -> bool {
        self.start <= requested.start && self.end >= requested.end
    }

    fn merge(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// One immutable segment descriptor in a manifest.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ManifestSegment {
    id: SegmentId,
    byte_length: NonZeroU64,
}

impl ManifestSegment {
    /// Names one nonempty immutable exact-key segment.
    #[must_use]
    pub const fn exact(id: ContentId<IndexExactSegmentDomain>, byte_length: NonZeroU64) -> Self {
        Self {
            id: SegmentId::Exact(id),
            byte_length,
        }
    }

    /// Names one nonempty immutable lexical segment.
    #[must_use]
    pub const fn lexical(
        id: ContentId<IndexLexicalSegmentDomain>,
        byte_length: NonZeroU64,
    ) -> Self {
        Self {
            id: SegmentId::Lexical(id),
            byte_length,
        }
    }

    /// Returns the family-specific segment identity.
    #[must_use]
    pub const fn id(self) -> SegmentId {
        self.id
    }

    /// Returns the full segment byte length.
    #[must_use]
    pub const fn byte_length(self) -> NonZeroU64 {
        self.byte_length
    }

    /// Returns the exact full immutable segment range.
    #[must_use]
    pub const fn full_range(self) -> SegmentRange {
        SegmentRange {
            start: 0,
            end: self.byte_length.get(),
        }
    }
}

/// One demand-selected segment range.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SegmentDemand {
    segment: SegmentId,
    range: SegmentRange,
}

impl SegmentDemand {
    /// Pairs one immutable segment with one exact requested range.
    #[must_use]
    pub const fn new(segment: SegmentId, range: SegmentRange) -> Self {
        Self { segment, range }
    }

    /// Returns the demanded segment identity.
    #[must_use]
    pub const fn segment(self) -> SegmentId {
        self.segment
    }

    /// Returns the demanded exact byte range.
    #[must_use]
    pub const fn range(self) -> SegmentRange {
        self.range
    }
}

/// Exact reason an immutable manifest was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManifestError {
    /// The exact lane exceeded its bounded immutable snapshot capacity.
    ExactSegmentLimit {
        /// Number of exact descriptors supplied.
        supplied: usize,
        /// Maximum exact descriptor capacity.
        maximum: usize,
    },
    /// The lexical lane exceeded its bounded immutable snapshot capacity.
    LexicalSegmentLimit {
        /// Number of lexical descriptors supplied.
        supplied: usize,
        /// Maximum lexical descriptor capacity.
        maximum: usize,
    },
    /// A descriptor appeared in the exact lane with the wrong family marker.
    ExactLaneFamily,
    /// A descriptor appeared in the lexical lane with the wrong family marker.
    LexicalLaneFamily,
    /// One exact segment identity appeared at two update-order positions.
    DuplicateExactSegment,
    /// One lexical segment identity appeared at two update-order positions.
    DuplicateLexicalSegment,
    /// The declared remote snapshot did not bind this canonical manifest lane.
    SnapshotMismatch {
        /// Snapshot derived from the generation and canonical exact/lexical descriptor lanes.
        expected: IndexSnapshotId,
        /// Snapshot authority supplied by the remote response.
        observed: IndexSnapshotId,
    },
    /// The shared portable snapshot identity grammar could not encode a validated lane count.
    Identity(IndexSnapshotIdentityError),
}

/// A fully validated remote immutable manifest owned by the client.
#[derive(Debug, Eq, PartialEq)]
pub struct ClientManifest {
    generation: RemoteGeneration,
    epoch: ManifestEpoch,
    exact: Box<[ManifestSegment]>,
    lexical: Box<[ManifestSegment]>,
    exact_lookup: ArrayVec<u8, MAX_CLIENT_SEGMENTS_PER_LANE>,
    lexical_lookup: ArrayVec<u8, MAX_CLIENT_SEGMENTS_PER_LANE>,
}

// A private sorted ordinal directory points back into a manifest's update-ordered lane. The
// manifest lanes deliberately retain the producer's newest-first/update order so snapshot
// identity remains byte-for-byte compatible with server admission. The directory stores only
// compact update ordinals rather than a second copy of every 32-byte identity; it is a bounded
// disposable structural projection, not a second manifest ordering or source of authority.

impl ClientManifest {
    /// Admits complete update-ordered exact and lexical lanes and binds them to the snapshot ID.
    ///
    /// Owning the caller's boxed descriptor lane avoids an additional copy while preserving the
    /// raw immutable segment backing outside this control-plane object.
    ///
    /// # Errors
    ///
    /// Returns a typed lane, duplicate, or snapshot-identity rejection without accepting either
    /// descriptor lane.
    pub fn from_lanes(
        generation: RemoteGeneration,
        epoch: ManifestEpoch,
        exact: Box<[ManifestSegment]>,
        lexical: Box<[ManifestSegment]>,
    ) -> Result<Self, ManifestError> {
        if exact.len() > MAX_CLIENT_SEGMENTS_PER_LANE {
            return Err(ManifestError::ExactSegmentLimit {
                supplied: exact.len(),
                maximum: MAX_CLIENT_SEGMENTS_PER_LANE,
            });
        }
        if lexical.len() > MAX_CLIENT_SEGMENTS_PER_LANE {
            return Err(ManifestError::LexicalSegmentLimit {
                supplied: lexical.len(),
                maximum: MAX_CLIENT_SEGMENTS_PER_LANE,
            });
        }
        if exact
            .iter()
            .any(|segment| !matches!(segment.id, SegmentId::Exact(_)))
        {
            return Err(ManifestError::ExactLaneFamily);
        }
        if lexical
            .iter()
            .any(|segment| !matches!(segment.id, SegmentId::Lexical(_)))
        {
            return Err(ManifestError::LexicalLaneFamily);
        }
        let mut exact_ids: ArrayVec<
            ContentId<IndexExactSegmentDomain>,
            MAX_CLIENT_SEGMENTS_PER_LANE,
        > = ArrayVec::new();
        for segment in &exact {
            let SegmentId::Exact(id) = segment.id else {
                return Err(ManifestError::ExactLaneFamily);
            };
            if exact_ids.try_push(id).is_err() {
                return Err(ManifestError::ExactSegmentLimit {
                    supplied: exact.len(),
                    maximum: MAX_CLIENT_SEGMENTS_PER_LANE,
                });
            }
        }
        let mut lexical_ids: ArrayVec<
            ContentId<IndexLexicalSegmentDomain>,
            MAX_CLIENT_SEGMENTS_PER_LANE,
        > = ArrayVec::new();
        for segment in &lexical {
            let SegmentId::Lexical(id) = segment.id else {
                return Err(ManifestError::LexicalLaneFamily);
            };
            if lexical_ids.try_push(id).is_err() {
                return Err(ManifestError::LexicalSegmentLimit {
                    supplied: lexical.len(),
                    maximum: MAX_CLIENT_SEGMENTS_PER_LANE,
                });
            }
        }
        let expected = derive_index_snapshot(generation.generation(), &exact_ids, &lexical_ids)
            .map_err(ManifestError::Identity)?;
        if expected != generation.snapshot() {
            return Err(ManifestError::SnapshotMismatch {
                expected,
                observed: generation.snapshot(),
            });
        }
        let exact_lookup = build_lookup(&exact, ManifestLane::Exact)?;
        let lexical_lookup = build_lookup(&lexical, ManifestLane::Lexical)?;
        Ok(Self {
            generation,
            epoch,
            exact,
            lexical,
            exact_lookup,
            lexical_lookup,
        })
    }

    /// Returns the still-unaccepted remote generation.
    #[must_use]
    pub const fn generation(&self) -> RemoteGeneration {
        self.generation
    }

    /// Returns the remote stream epoch.
    #[must_use]
    pub const fn epoch(&self) -> ManifestEpoch {
        self.epoch
    }

    /// Borrows the validated exact segment descriptors in immutable update order.
    #[must_use]
    pub const fn exact(&self) -> &[ManifestSegment] {
        &self.exact
    }

    /// Borrows the validated lexical segment descriptors in immutable update order.
    #[must_use]
    pub const fn lexical(&self) -> &[ManifestSegment] {
        &self.lexical
    }

    fn find(&self, id: SegmentId) -> Option<&ManifestSegment> {
        let (lane, lookup) = match id {
            SegmentId::Exact(_) => (&self.exact, &self.exact_lookup),
            SegmentId::Lexical(_) => (&self.lexical, &self.lexical_lookup),
        };
        find_in_lookup(lane, lookup, id)
    }

    fn all_segments(&self) -> impl Iterator<Item = &ManifestSegment> {
        self.exact.iter().chain(self.lexical.iter())
    }
}

#[derive(Clone, Copy)]
enum ManifestLane {
    Exact,
    Lexical,
}

fn build_lookup(
    segments: &[ManifestSegment],
    lane: ManifestLane,
) -> Result<ArrayVec<u8, MAX_CLIENT_SEGMENTS_PER_LANE>, ManifestError> {
    let mut lookup = ArrayVec::new();
    for update_index in 0..segments.len() {
        let update_index = u8::try_from(update_index).map_err(|_| match lane {
            ManifestLane::Exact => ManifestError::ExactSegmentLimit {
                supplied: segments.len(),
                maximum: MAX_CLIENT_SEGMENTS_PER_LANE,
            },
            ManifestLane::Lexical => ManifestError::LexicalSegmentLimit {
                supplied: segments.len(),
                maximum: MAX_CLIENT_SEGMENTS_PER_LANE,
            },
        })?;
        if lookup.try_push(update_index).is_err() {
            return Err(match lane {
                ManifestLane::Exact => ManifestError::ExactSegmentLimit {
                    supplied: segments.len(),
                    maximum: MAX_CLIENT_SEGMENTS_PER_LANE,
                },
                ManifestLane::Lexical => ManifestError::LexicalSegmentLimit {
                    supplied: segments.len(),
                    maximum: MAX_CLIENT_SEGMENTS_PER_LANE,
                },
            });
        }
    }
    lookup.sort_unstable_by(|left, right| {
        let Some(left) = segments.get(usize::from(*left)) else {
            return core::cmp::Ordering::Equal;
        };
        let Some(right) = segments.get(usize::from(*right)) else {
            return core::cmp::Ordering::Equal;
        };
        left.id.cmp(&right.id)
    });
    if lookup.windows(2).any(|pair| {
        let Some(left) = pair.first() else {
            return false;
        };
        let Some(right) = pair.get(1) else {
            return false;
        };
        let Some(left) = segments.get(usize::from(*left)) else {
            return false;
        };
        let Some(right) = segments.get(usize::from(*right)) else {
            return false;
        };
        left.id == right.id
    }) {
        return Err(match lane {
            ManifestLane::Exact => ManifestError::DuplicateExactSegment,
            ManifestLane::Lexical => ManifestError::DuplicateLexicalSegment,
        });
    }
    Ok(lookup)
}

fn find_in_lookup<'manifest>(
    lane: &'manifest [ManifestSegment],
    lookup: &[u8],
    target: SegmentId,
) -> Option<&'manifest ManifestSegment> {
    let mut lower = 0_usize;
    let mut upper = lookup.len();
    while lower < upper {
        let middle = lower + (upper - lower) / 2;
        let ordinal = *lookup.get(middle)?;
        let candidate = lane.get(usize::from(ordinal))?;
        match candidate.id.cmp(&target) {
            core::cmp::Ordering::Less => lower = middle + 1,
            core::cmp::Ordering::Greater => upper = middle,
            core::cmp::Ordering::Equal => return Some(candidate),
        }
    }
    None
}

/// A remote manifest response that has not yet changed local canonical state.
#[derive(Debug, Eq, PartialEq)]
pub enum RemoteManifest {
    /// The first complete manifest for an unpinned client.
    Initial(ClientManifest),
    /// A complete successor that names the remote base it extends.
    Advance {
        /// The remote generation the response claims to extend.
        previous: RemoteGeneration,
        /// The complete successor manifest.
        next: ClientManifest,
    },
    /// The remote could not provide a complete manifest for this generation.
    Partial {
        /// The generation for which manifest bytes remain incomplete.
        generation: RemoteGeneration,
    },
}

impl RemoteManifest {
    /// Wraps the first complete remote manifest response.
    #[must_use]
    pub const fn initial(manifest: ClientManifest) -> Self {
        Self::Initial(manifest)
    }

    /// Wraps a complete remote successor with its explicit predecessor authority.
    #[must_use]
    pub const fn advance(previous: RemoteGeneration, next: ClientManifest) -> Self {
        Self::Advance { previous, next }
    }

    /// Reports a remote partial-manifest terminal without inventing an empty success.
    #[must_use]
    pub const fn partial(generation: RemoteGeneration) -> Self {
        Self::Partial { generation }
    }
}

/// A monotonic revision of locally observed code or environment changes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OverlayGeneration(NonZeroU64);

impl OverlayGeneration {
    /// Returns the opaque local overlay revision for protocol observations.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    const fn first() -> Self {
        // One is a fixed nonzero representation value.
        Self(NonZeroU64::MIN)
    }

    const fn next(self) -> Result<Self, ClientSyncError> {
        let Some(next) = self.0.get().checked_add(1) else {
            return Err(ClientSyncError::OverlayGenerationExhausted);
        };
        // `next` is produced by adding one to a nonzero integer.
        let Some(next) = NonZeroU64::new(next) else {
            return Err(ClientSyncError::OverlayGenerationExhausted);
        };
        Ok(Self(next))
    }
}

/// A local code or environment key reconciled independently of remote segment generations.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OverlayKey(ContentId<ObjectDomain>);

impl OverlayKey {
    /// Wraps one domain-separated local code or environment identity.
    #[must_use]
    pub const fn new(value: ContentId<ObjectDomain>) -> Self {
        Self(value)
    }

    /// Returns the domain-separated local key identity.
    #[must_use]
    pub const fn value(self) -> ContentId<ObjectDomain> {
        self.0
    }
}

/// One local overlay mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalDelta {
    /// A local code or environment value supersedes the remote base.
    Upsert {
        /// Local semantic key.
        key: OverlayKey,
        /// Content identity of the locally observed replacement.
        value: ContentId<ObjectDomain>,
    },
    /// A local deletion supersedes the remote base until reconciliation chooses a new overlay.
    Tombstone {
        /// Local semantic key deleted by the client environment.
        key: OverlayKey,
    },
}

impl LocalDelta {
    /// Creates a local upsert delta.
    #[must_use]
    pub const fn upsert(key: OverlayKey, value: ContentId<ObjectDomain>) -> Self {
        Self::Upsert { key, value }
    }

    /// Creates a local deletion delta.
    #[must_use]
    pub const fn tombstone(key: OverlayKey) -> Self {
        Self::Tombstone { key }
    }

    const fn key(self) -> OverlayKey {
        match self {
            Self::Upsert { key, .. } | Self::Tombstone { key } => key,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverlayFact {
    Upsert(ContentId<ObjectDomain>),
    Tombstone,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OverlayEntry {
    key: OverlayKey,
    fact: OverlayFact,
}

/// Closed observation of a local overlay key after reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayObservation {
    /// The local key names a current replacement value.
    Upsert {
        /// Content identity of the local replacement.
        value: ContentId<ObjectDomain>,
    },
    /// The local key remains deleted regardless of remote manifest movement.
    Tombstone,
    /// No local overlay exists for this key.
    Absent,
}

/// Exact state of the client synchronization controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientSyncPhase {
    /// No complete remote manifest has been accepted.
    AwaitingManifest,
    /// A complete manifest is pinned while immutable segment/range acquisition is in progress.
    Synchronizing {
        /// Accepted local base authority.
        base: BaseGeneration,
    },
    /// The pinned manifest is locally queryable for its demanded segment ranges.
    LocalReady {
        /// Accepted local base authority.
        base: BaseGeneration,
    },
    /// Remote transport is unavailable; retained canonical facts and overlay remain usable.
    Disconnected {
        /// Accepted local base authority.
        base: BaseGeneration,
    },
}

/// An explicit cancellation sample for synchronous client control-plane work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncCancellation {
    /// The caller permits this bounded operation to continue.
    Continue,
    /// The caller cancelled before any output or client state can change.
    Cancelled,
}

/// Exact result of one remote-manifest transition attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncTerminal {
    /// The client cannot accept a successor or finish synchronization before an initial manifest.
    AwaitingManifest,
    /// The first complete manifest became the client's pinned base.
    AcceptedInitial {
        /// Newly accepted local base authority.
        base: BaseGeneration,
    },
    /// A complete successor replaced the pinned base while preserving overlay facts.
    AcceptedAdvance {
        /// Prior local base authority.
        previous: BaseGeneration,
        /// Newly accepted local base authority.
        next: BaseGeneration,
    },
    /// A bounded local selection proved all of its demanded ranges resident for this base.
    LocalReady {
        /// Accepted base authority proved by the complete local selection.
        base: BaseGeneration,
    },
    /// A complete local-selection proof belonged to a base replaced before readiness transition.
    LocalSelectionStale {
        /// Current accepted base authority.
        accepted: BaseGeneration,
        /// Base authority carried by the completed local selection proof.
        proved: BaseGeneration,
    },
    /// A selection was complete, but the current canonical manifest is not fully locally resident.
    LocalSelectionIncomplete {
        /// Accepted base whose canonical full ranges remain partially absent.
        base: BaseGeneration,
    },
    /// The remote response was older than or equal to the current accepted manifest.
    Stale {
        /// Current accepted base authority.
        accepted: BaseGeneration,
        /// Stale remote authority.
        observed: RemoteGeneration,
    },
    /// The remote response did not name the client's current base as its predecessor.
    OutOfOrder {
        /// Current accepted base authority.
        expected: BaseGeneration,
        /// Remote predecessor authority supplied by the response.
        observed: RemoteGeneration,
    },
    /// The remote did not provide a complete manifest and local state was unchanged.
    Partial {
        /// Remote generation whose manifest remains incomplete.
        generation: RemoteGeneration,
    },
    /// The operation was cancelled without changing client state.
    Cancelled,
}

/// Exact control-plane failure that cannot be represented as a normal terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientSyncError {
    /// A local overlay revision exhausted the protocol coordinate width.
    OverlayGenerationExhausted,
    /// A recorded resident range does not fit the immutable manifest segment.
    ResidentRangeOutsideManifest,
    /// A demand selection exceeds the bounded caller-owned scratch contract.
    TooManyDemands {
        /// Number of demands supplied by the caller.
        supplied: usize,
        /// Maximum accepted demand count.
        maximum: usize,
    },
    /// Demands must retain the stable strict order used for deterministic bounded selection.
    UnsortedOrDuplicateDemand,
    /// The caller's scratch cannot hold every requested selection result.
    InsufficientScratch {
        /// Number of demanded ranges.
        required: usize,
        /// Number of supplied scratch slots.
        available: usize,
    },
    /// The caller's output cannot hold every requested selection result.
    InsufficientOutput {
        /// Number of demanded ranges.
        required: usize,
        /// Number of supplied output slots.
        available: usize,
    },
    /// A remote response named a compiler generation other than the pinned generation.
    RemoteSearchGenerationMismatch {
        /// Current accepted compiler generation.
        expected: GenerationId,
        /// Remote compiler generation carried by the candidate response.
        observed: GenerationId,
    },
    /// A remote response named a different snapshot for the same compiler generation.
    RemoteSearchSnapshotMismatch {
        /// Current accepted snapshot identity.
        expected: IndexSnapshotId,
        /// Remote snapshot identity carried by the candidate response.
        observed: IndexSnapshotId,
    },
    /// A remote search arrived before a complete remote manifest was accepted.
    RemoteSearchBeforeManifest,
    /// Local range selection was requested before a complete remote manifest was accepted.
    LocalSelectionBeforeManifest,
    /// Local segment residence cannot be recorded before a complete manifest is pinned.
    ResidentRangeBeforeManifest,
    /// A requested range exceeds the immutable segment bounds named by the manifest.
    DemandRangeOutsideManifest,
    /// Local overlay storage reached its explicit compact-client bound.
    OverlayCapacity {
        /// Maximum retained overlay entries.
        maximum: usize,
    },
    /// Local immutable range receipts reached their explicit compact-client bound.
    ResidenceCapacity {
        /// Maximum retained resident ranges.
        maximum: usize,
    },
    /// Disposable projection records reached their explicit compact-client bound.
    ProjectionCapacity {
        /// Maximum retained disposable projections.
        maximum: usize,
    },
}

#[derive(Debug, Eq, PartialEq)]
enum ClientState {
    AwaitingManifest,
    Pinned {
        manifest: ClientManifest,
        state: PinnedState,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PinnedState {
    Synchronizing,
    LocalReady,
    Disconnected,
}

impl PinnedState {
    const fn phase(self, base: BaseGeneration) -> ClientSyncPhase {
        match self {
            Self::Synchronizing => ClientSyncPhase::Synchronizing { base },
            Self::LocalReady => ClientSyncPhase::LocalReady { base },
            Self::Disconnected => ClientSyncPhase::Disconnected { base },
        }
    }
}

/// A locally retained exact immutable range. The payload owner remains outside this control plane.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResidentRange {
    segment: SegmentId,
    range: SegmentRange,
}

impl ResidentRange {
    /// Names one exact immutable segment range retained by the local store.
    #[must_use]
    pub const fn new(segment: SegmentId, range: SegmentRange) -> Self {
        Self { segment, range }
    }

    /// Returns the retained immutable segment identity.
    #[must_use]
    pub const fn segment(self) -> SegmentId {
        self.segment
    }

    /// Returns the retained exact immutable byte range.
    #[must_use]
    pub const fn range(self) -> SegmentRange {
        self.range
    }
}

/// A borrowed selected immutable segment range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalSegmentSelection<'manifest> {
    segment: &'manifest ManifestSegment,
    range: SegmentRange,
}

impl<'manifest> LocalSegmentSelection<'manifest> {
    /// Returns the borrowed validated manifest descriptor.
    #[must_use]
    pub const fn segment(self) -> &'manifest ManifestSegment {
        self.segment
    }

    /// Returns the exact requested range without copying payload bytes.
    #[must_use]
    pub const fn range(self) -> SegmentRange {
        self.range
    }
}

/// One completed local selection slot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalSelection<'manifest> {
    /// The caller's local immutable store covers this exact range.
    Present(LocalSegmentSelection<'manifest>),
    /// The manifest selected this exact range but the local store has not retained it.
    Missing(ResidentRange),
}

/// A validated borrowed demand lane, strictly ordered for one linear selection pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DemandSelection<'selection> {
    demands: &'selection [SegmentDemand],
}

impl<'selection> DemandSelection<'selection> {
    /// Validates a bounded, strictly ordered caller-owned demand lane.
    ///
    /// # Errors
    ///
    /// Returns a typed capacity or ordering error without admitting the demand lane.
    pub fn new(demands: &'selection [SegmentDemand]) -> Result<Self, ClientSyncError> {
        if demands.len() > MAX_CLIENT_DEMANDS {
            return Err(ClientSyncError::TooManyDemands {
                supplied: demands.len(),
                maximum: MAX_CLIENT_DEMANDS,
            });
        }
        if demands.windows(2).any(|pair| {
            let Some(previous) = pair.first() else {
                return false;
            };
            let Some(next) = pair.get(1) else {
                return false;
            };
            previous >= next
        }) {
            return Err(ClientSyncError::UnsortedOrDuplicateDemand);
        }
        Ok(Self { demands })
    }

    /// Borrows the already validated requested ranges.
    #[must_use]
    pub const fn demands(self) -> &'selection [SegmentDemand] {
        self.demands
    }
}

/// Caller-owned temporary slots used to preserve transactional selection output.
pub struct SelectionScratch<'scratch, 'manifest> {
    slots: &'scratch mut [Option<LocalSelection<'manifest>>],
}

impl<'scratch, 'manifest> SelectionScratch<'scratch, 'manifest> {
    /// Lends reusable selection slots without making the client retain a query allocation.
    #[must_use]
    pub const fn new(slots: &'scratch mut [Option<LocalSelection<'manifest>>]) -> Self {
        Self { slots }
    }

    const fn into_slots(self) -> &'scratch mut [Option<LocalSelection<'manifest>>] {
        self.slots
    }
}

/// Caller-owned committed selection slots.
pub struct SelectionOutput<'output, 'manifest> {
    slots: &'output mut [Option<LocalSelection<'manifest>>],
}

/// A non-forgeable completion witness produced only by a fully resident local selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompleteLocalSelection {
    base: BaseGeneration,
}

impl<'output, 'manifest> SelectionOutput<'output, 'manifest> {
    /// Lends output slots that change only after the entire selection preflight succeeds.
    #[must_use]
    pub const fn new(slots: &'output mut [Option<LocalSelection<'manifest>>]) -> Self {
        Self { slots }
    }

    const fn into_slots(self) -> &'output mut [Option<LocalSelection<'manifest>>] {
        self.slots
    }
}

/// Honest terminal for one borrowed local selection.
#[derive(Debug, Eq, PartialEq)]
pub enum LocalQueryTerminal<'output, 'manifest> {
    /// Every selected range is locally retained.
    Complete {
        /// One stable output slot per input demand.
        selections: &'output [Option<LocalSelection<'manifest>>],
        /// Completion witness accepted by [`ClientIndex::complete_sync`].
        proof: CompleteLocalSelection,
    },
    /// Some selected immutable ranges remain absent and are named exactly.
    Partial {
        /// One stable output slot per input demand.
        selections: &'output [Option<LocalSelection<'manifest>>],
        /// Exact count of `Missing` slots in `selections`.
        missing: usize,
    },
    /// The operation was cancelled before either scratch or output was changed.
    Cancelled,
}

/// One remote query result that is candidate evidence only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteSearchCandidate {
    key: OverlayKey,
    document: EntityDocumentId,
}

impl RemoteSearchCandidate {
    /// Names one remote candidate document without source-span authority.
    #[must_use]
    pub const fn new(key: OverlayKey, document: EntityDocumentId) -> Self {
        Self { key, document }
    }

    /// Returns the semantic key on which a local overlay may supersede this remote candidate.
    #[must_use]
    pub const fn key(self) -> OverlayKey {
        self.key
    }

    /// Returns the candidate's canonical document identity.
    #[must_use]
    pub const fn document(self) -> EntityDocumentId {
        self.document
    }
}

/// Effective document identity after reconciling immutable remote and local-unindexed facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectiveSearchDocument {
    /// A remote candidate retained its canonical artifact-and-entity identity.
    RemoteEntity(EntityDocumentId),
    /// A local code or environment overlay has not yet been admitted to an immutable entity image.
    LocalObject(ContentId<ObjectDomain>),
}

/// One query result after the local code/environment overlay reconciles a remote candidate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectiveSearchResult {
    /// The remote candidate was not locally superseded.
    Remote(RemoteSearchCandidate),
    /// A local code or environment update superseded the remote candidate value.
    LocalOverlay {
        /// Semantic key shared with the remote candidate.
        key: OverlayKey,
        /// Locally observed replacement content identity.
        document: ContentId<ObjectDomain>,
    },
}

impl EffectiveSearchResult {
    /// Returns the semantic key selected by this reconciled result.
    #[must_use]
    pub const fn key(self) -> OverlayKey {
        match self {
            Self::Remote(candidate) => candidate.key,
            Self::LocalOverlay { key, .. } => key,
        }
    }

    /// Returns the effective document identity after local overlay reconciliation.
    #[must_use]
    pub const fn document(self) -> EffectiveSearchDocument {
        match self {
            Self::Remote(candidate) => EffectiveSearchDocument::RemoteEntity(candidate.document),
            Self::LocalOverlay { document, .. } => EffectiveSearchDocument::LocalObject(document),
        }
    }
}

/// A borrowed remote search response, valid only for its advertised immutable generation.
#[derive(Debug, Eq, PartialEq)]
pub struct RemoteSearch<'candidates> {
    generation: RemoteGeneration,
    candidates: &'candidates [RemoteSearchCandidate],
}

impl<'candidates> RemoteSearch<'candidates> {
    /// Borrows remote candidate evidence without attaching source provenance or retaining storage.
    #[must_use]
    pub const fn new(
        generation: RemoteGeneration,
        candidates: &'candidates [RemoteSearchCandidate],
    ) -> Self {
        Self {
            generation,
            candidates,
        }
    }

    /// Returns the remote response authority.
    #[must_use]
    pub const fn generation(&self) -> RemoteGeneration {
        self.generation
    }

    /// Borrows candidate evidence for a remote response.
    #[must_use]
    pub const fn candidates(&self) -> &'candidates [RemoteSearchCandidate] {
        self.candidates
    }
}

/// Exact terminal for immediate remote candidate search.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteSearchTerminal {
    /// Candidate results arrived while local immutable range synchronization may still continue.
    Candidates {
        /// Accepted client base matching the remote response.
        base: BaseGeneration,
        /// Number of copied candidate result slots.
        returned: usize,
    },
}

/// Disposable derived client projection kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisposableProjection {
    /// A locally rebuilt lexical acceleration structure.
    Lexical,
    /// A locally rebuilt result-ordering or snippet cache.
    QueryCache,
}

/// Client-owned local-first state with a pinned manifest, independent overlay, and range receipts.
#[derive(Debug, Eq, PartialEq)]
pub struct ClientIndex {
    state: ClientState,
    overlay_generation: OverlayGeneration,
    overlay: ArrayVec<OverlayEntry, MAX_CLIENT_OVERLAY_ENTRIES>,
    residence: ArrayVec<ResidentRange, MAX_CLIENT_RESIDENT_RANGES>,
    projections: ArrayVec<DisposableProjection, MAX_CLIENT_PROJECTIONS>,
}

impl Default for ClientIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientIndex {
    /// Creates an empty client that owns no remote manifest but can retain local deltas immediately.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: ClientState::AwaitingManifest,
            overlay_generation: OverlayGeneration::first(),
            overlay: ArrayVec::new(),
            residence: ArrayVec::new(),
            projections: ArrayVec::new(),
        }
    }

    /// Returns the closed synchronization phase without exposing mutable state combinations.
    #[must_use]
    pub const fn phase(&self) -> ClientSyncPhase {
        match &self.state {
            ClientState::AwaitingManifest => ClientSyncPhase::AwaitingManifest,
            ClientState::Pinned { manifest, state } => state.phase(manifest.generation.accepted()),
        }
    }

    /// Returns the current local overlay revision independent from the accepted remote base.
    #[must_use]
    pub const fn overlay_generation(&self) -> OverlayGeneration {
        self.overlay_generation
    }

    /// Applies one local code or environment delta without altering remote canonical state.
    ///
    /// # Errors
    ///
    /// Returns a typed generation or bounded-overlay capacity error before changing controller
    /// state.
    pub fn record_local_delta(
        &mut self,
        delta: LocalDelta,
    ) -> Result<OverlayGeneration, ClientSyncError> {
        let next_generation = self.overlay_generation.next()?;
        let key = delta.key();
        let fact = match delta {
            LocalDelta::Upsert { value, .. } => OverlayFact::Upsert(value),
            LocalDelta::Tombstone { .. } => OverlayFact::Tombstone,
        };
        let insertion = self.overlay.binary_search_by_key(&key, |entry| entry.key);
        if insertion.is_err() && self.overlay.len() == MAX_CLIENT_OVERLAY_ENTRIES {
            return Err(ClientSyncError::OverlayCapacity {
                maximum: MAX_CLIENT_OVERLAY_ENTRIES,
            });
        }
        match insertion {
            Ok(index) => {
                let Some(entry) = self.overlay.get_mut(index) else {
                    return Err(ClientSyncError::OverlayCapacity {
                        maximum: MAX_CLIENT_OVERLAY_ENTRIES,
                    });
                };
                entry.fact = fact;
            }
            Err(index) => {
                if self
                    .overlay
                    .try_insert(index, OverlayEntry { key, fact })
                    .is_err()
                {
                    return Err(ClientSyncError::OverlayCapacity {
                        maximum: MAX_CLIENT_OVERLAY_ENTRIES,
                    });
                }
            }
        }
        self.overlay_generation = next_generation;
        Ok(self.overlay_generation)
    }

    /// Looks up the local overlay without consulting or mutating the remote base manifest.
    #[must_use]
    pub fn overlay(&self, key: OverlayKey) -> OverlayObservation {
        let Ok(index) = self.overlay.binary_search_by_key(&key, |entry| entry.key) else {
            return OverlayObservation::Absent;
        };
        let Some(entry) = self.overlay.get(index) else {
            return OverlayObservation::Absent;
        };
        match entry.fact {
            OverlayFact::Upsert(value) => OverlayObservation::Upsert { value },
            OverlayFact::Tombstone => OverlayObservation::Tombstone,
        }
    }

    /// Applies one complete remote manifest terminal without discarding local overlay or receipts.
    pub fn accept_manifest(
        &mut self,
        remote: RemoteManifest,
        cancellation: SyncCancellation,
    ) -> SyncTerminal {
        if cancellation == SyncCancellation::Cancelled {
            return SyncTerminal::Cancelled;
        }
        match (&self.state, remote) {
            (_, RemoteManifest::Partial { generation }) => SyncTerminal::Partial { generation },
            (ClientState::AwaitingManifest, RemoteManifest::Initial(manifest)) => {
                let base = manifest.generation.accepted();
                self.state = ClientState::Pinned {
                    manifest,
                    state: PinnedState::Synchronizing,
                };
                SyncTerminal::AcceptedInitial { base }
            }
            (
                ClientState::Pinned {
                    manifest: accepted, ..
                },
                RemoteManifest::Initial(manifest),
            ) => SyncTerminal::Stale {
                accepted: accepted.generation.accepted(),
                observed: manifest.generation,
            },
            (ClientState::AwaitingManifest, RemoteManifest::Advance { .. }) => {
                SyncTerminal::AwaitingManifest
            }
            (
                ClientState::Pinned {
                    manifest: accepted, ..
                },
                RemoteManifest::Advance { previous, next },
            ) => {
                let current = accepted.generation.accepted();
                if !previous.matches(current) {
                    return SyncTerminal::OutOfOrder {
                        expected: current,
                        observed: previous,
                    };
                }
                if next.generation.matches(current) || next.epoch <= accepted.epoch {
                    return SyncTerminal::Stale {
                        accepted: current,
                        observed: next.generation,
                    };
                }
                let successor = next.generation.accepted();
                self.state = ClientState::Pinned {
                    manifest: next,
                    state: PinnedState::Synchronizing,
                };
                SyncTerminal::AcceptedAdvance {
                    previous: current,
                    next: successor,
                }
            }
        }
    }

    /// Marks the currently pinned manifest as locally current after its background range work ends.
    ///
    /// This transition changes no canonical manifest, local overlay, or range receipt.
    pub fn complete_sync(
        &mut self,
        proof: CompleteLocalSelection,
        cancellation: SyncCancellation,
    ) -> SyncTerminal {
        if cancellation == SyncCancellation::Cancelled {
            return SyncTerminal::Cancelled;
        }
        match &mut self.state {
            ClientState::AwaitingManifest => SyncTerminal::AwaitingManifest,
            ClientState::Pinned { manifest, state } => {
                let base = manifest.generation.accepted();
                if proof.base != base {
                    return SyncTerminal::LocalSelectionStale {
                        accepted: base,
                        proved: proof.base,
                    };
                }
                if !manifest.all_segments().copied().all(|segment| {
                    residence_covers(
                        &self.residence,
                        SegmentDemand::new(segment.id, segment.full_range()),
                    )
                }) {
                    return SyncTerminal::LocalSelectionIncomplete { base };
                }
                *state = PinnedState::LocalReady;
                SyncTerminal::LocalReady { base }
            }
        }
    }

    fn coalesced_residence(
        &self,
        resident: ResidentRange,
    ) -> Result<ArrayVec<ResidentRange, MAX_CLIENT_RESIDENT_RANGES>, ClientSyncError> {
        let mut next = ArrayVec::new();
        let mut merged = resident;
        let mut inserted = false;
        for present in self.residence.iter().copied() {
            if inserted {
                push_resident(&mut next, present)?;
                continue;
            }
            match present.segment.cmp(&merged.segment) {
                core::cmp::Ordering::Less => push_resident(&mut next, present)?,
                core::cmp::Ordering::Greater => {
                    push_resident(&mut next, merged)?;
                    push_resident(&mut next, present)?;
                    inserted = true;
                }
                core::cmp::Ordering::Equal => {
                    if present.range.end() < merged.range.start() {
                        push_resident(&mut next, present)?;
                    } else if merged.range.end() < present.range.start() {
                        push_resident(&mut next, merged)?;
                        push_resident(&mut next, present)?;
                        inserted = true;
                    } else {
                        merged.range = merged.range.merge(present.range);
                    }
                }
            }
        }
        if !inserted {
            push_resident(&mut next, merged)?;
        }
        Ok(next)
    }

    fn residence_covers(&self, demand: SegmentDemand) -> bool {
        residence_covers(&self.residence, demand)
    }

    /// Records a locally retained exact range after checking it against the currently pinned manifest.
    ///
    /// Receipts are kept sorted by `(segment, start)` and coalesced when adjacent or overlapping,
    /// so coverage checks require a single partition lookup rather than scanning all receipts.
    ///
    /// # Errors
    ///
    /// Returns a typed manifest-bound or residence-capacity error before changing retained
    /// receipts.
    pub fn record_resident_range(
        &mut self,
        resident: ResidentRange,
    ) -> Result<(), ClientSyncError> {
        let ClientState::Pinned { manifest, .. } = &self.state else {
            return Err(ClientSyncError::ResidentRangeBeforeManifest);
        };
        let Some(segment) = manifest.find(resident.segment) else {
            return Err(ClientSyncError::ResidentRangeOutsideManifest);
        };
        if !segment.full_range().contains(resident.range) {
            return Err(ClientSyncError::ResidentRangeOutsideManifest);
        }
        let next = self.coalesced_residence(resident)?;
        self.residence = next;
        Ok(())
    }

    /// Records a disposable projection that may later be evicted without changing canonical facts.
    ///
    /// # Errors
    ///
    /// Returns [`ClientSyncError::ProjectionCapacity`] if no bounded projection slot remains.
    pub fn record_disposable_projection(
        &mut self,
        projection: DisposableProjection,
    ) -> Result<(), ClientSyncError> {
        if self.projections.contains(&projection) {
            return Ok(());
        }
        if self.projections.len() == MAX_CLIENT_PROJECTIONS {
            return Err(ClientSyncError::ProjectionCapacity {
                maximum: MAX_CLIENT_PROJECTIONS,
            });
        }
        if self.projections.try_push(projection).is_err() {
            return Err(ClientSyncError::ProjectionCapacity {
                maximum: MAX_CLIENT_PROJECTIONS,
            });
        }
        Ok(())
    }

    /// Drops only disposable local projections and returns the exact number evicted.
    pub fn evict_disposable_projections(&mut self) -> usize {
        let evicted = self.projections.len();
        self.projections.clear();
        evicted
    }

    /// Enters a remote-outage phase while retaining the pinned manifest, all range receipts, and overlay.
    pub const fn disconnect(&mut self) {
        if let ClientState::Pinned { state, .. } = &mut self.state {
            *state = PinnedState::Disconnected;
        }
    }

    /// Resumes background synchronization without discarding retained canonical local capability.
    pub const fn reconnect(&mut self) {
        if let ClientState::Pinned { state, .. } = &mut self.state {
            *state = PinnedState::Synchronizing;
        }
    }

    /// Selects locally retained immutable ranges through caller-owned scratch and transactional output.
    ///
    /// # Errors
    ///
    /// Every capacity or manifest-bound failure occurs before either scratch or output changes.
    pub fn select_local<'index, 'output>(
        &'index self,
        selection: DemandSelection<'_>,
        cancellation: SyncCancellation,
        scratch: SelectionScratch<'_, 'index>,
        output: SelectionOutput<'output, 'index>,
    ) -> Result<LocalQueryTerminal<'output, 'index>, ClientSyncError> {
        if cancellation == SyncCancellation::Cancelled {
            return Ok(LocalQueryTerminal::Cancelled);
        }
        let ClientState::Pinned { manifest, .. } = &self.state else {
            return Err(ClientSyncError::LocalSelectionBeforeManifest);
        };
        let demand_count = selection.demands.len();
        let scratch_slots = scratch.into_slots();
        let scratch_available = scratch_slots.len();
        let Some(scratch_slots) = scratch_slots.get_mut(..demand_count) else {
            return Err(ClientSyncError::InsufficientScratch {
                required: demand_count,
                available: scratch_available,
            });
        };
        let output_slots = output.into_slots();
        let output_available = output_slots.len();
        let Some(output_slots) = output_slots.get_mut(..demand_count) else {
            return Err(ClientSyncError::InsufficientOutput {
                required: demand_count,
                available: output_available,
            });
        };
        let mut selected_segments = ArrayVec::<&ManifestSegment, MAX_CLIENT_DEMANDS>::new();
        for demand in selection.demands {
            let Some(segment) = manifest.find(demand.segment) else {
                return Err(ClientSyncError::DemandRangeOutsideManifest);
            };
            if !segment.full_range().contains(demand.range) {
                return Err(ClientSyncError::DemandRangeOutsideManifest);
            }
            if selected_segments.try_push(segment).is_err() {
                return Err(ClientSyncError::TooManyDemands {
                    supplied: demand_count,
                    maximum: MAX_CLIENT_DEMANDS,
                });
            }
        }

        let mut missing = 0;
        for ((slot, demand), segment) in scratch_slots
            .iter_mut()
            .zip(selection.demands)
            .zip(selected_segments)
        {
            let resident = self.residence_covers(*demand);
            *slot = Some(if resident {
                LocalSelection::Present(LocalSegmentSelection {
                    segment,
                    range: demand.range,
                })
            } else {
                missing += 1;
                LocalSelection::Missing(ResidentRange::new(demand.segment, demand.range))
            });
        }
        output_slots.copy_from_slice(scratch_slots);
        let selections = &*output_slots;
        if missing == 0 {
            Ok(LocalQueryTerminal::Complete {
                selections,
                proof: CompleteLocalSelection {
                    base: manifest.generation.accepted(),
                },
            })
        } else {
            Ok(LocalQueryTerminal::Partial {
                selections,
                missing,
            })
        }
    }

    /// Copies immediate remote candidate evidence only when it matches the accepted immutable base.
    ///
    /// Candidate responses contain no source span and therefore cannot manufacture trusted source
    /// provenance while local synchronization is still running.
    ///
    /// # Errors
    ///
    /// Returns a typed base-authority or caller-output capacity error without retaining reply
    /// storage or changing the controller.
    pub fn accept_remote_search(
        &self,
        search: &RemoteSearch<'_>,
        output: &mut [Option<EffectiveSearchResult>],
    ) -> Result<RemoteSearchTerminal, ClientSyncError> {
        let ClientState::Pinned { manifest, .. } = &self.state else {
            return Err(ClientSyncError::RemoteSearchBeforeManifest);
        };
        let base = manifest.generation.accepted();
        if search.generation.generation() != base.generation() {
            return Err(ClientSyncError::RemoteSearchGenerationMismatch {
                expected: base.generation(),
                observed: search.generation.generation(),
            });
        }
        if search.generation.snapshot() != base.snapshot() {
            return Err(ClientSyncError::RemoteSearchSnapshotMismatch {
                expected: base.snapshot(),
                observed: search.generation.snapshot(),
            });
        }
        let Some(response_slots) = output.get_mut(..search.candidates.len()) else {
            return Err(ClientSyncError::InsufficientOutput {
                required: search.candidates.len(),
                available: output.len(),
            });
        };
        let effects = search.candidates.iter().copied().filter_map(|candidate| {
            match self.overlay(candidate.key) {
                OverlayObservation::Absent => Some(EffectiveSearchResult::Remote(candidate)),
                OverlayObservation::Upsert { value } => Some(EffectiveSearchResult::LocalOverlay {
                    key: candidate.key,
                    document: value,
                }),
                OverlayObservation::Tombstone => None,
            }
        });
        let mut returned = 0;
        for (slot, effective) in response_slots.iter_mut().zip(effects) {
            *slot = Some(effective);
            returned += 1;
        }
        for slot in response_slots.iter_mut().skip(returned) {
            *slot = None;
        }
        Ok(RemoteSearchTerminal::Candidates { base, returned })
    }
}

fn push_resident(
    residence: &mut ArrayVec<ResidentRange, MAX_CLIENT_RESIDENT_RANGES>,
    resident: ResidentRange,
) -> Result<(), ClientSyncError> {
    residence
        .try_push(resident)
        .map_err(|_| ClientSyncError::ResidenceCapacity {
            maximum: MAX_CLIENT_RESIDENT_RANGES,
        })
}

fn residence_covers(
    residence: &ArrayVec<ResidentRange, MAX_CLIENT_RESIDENT_RANGES>,
    demand: SegmentDemand,
) -> bool {
    let position = residence.partition_point(|resident| {
        resident.segment < demand.segment
            || (resident.segment == demand.segment
                && resident.range.start() <= demand.range.start())
    });
    let Some(previous) = position.checked_sub(1) else {
        return false;
    };
    let Some(resident) = residence.get(previous) else {
        return false;
    };
    resident.segment == demand.segment && resident.range.contains(demand.range)
}

#[cfg(test)]
mod layout_tests {
    use core::mem::size_of;

    use super::ClientManifest;

    #[test]
    fn manifest_lookup_directory_remains_compact() {
        let bytes = size_of::<ClientManifest>();
        assert!(
            bytes <= 128,
            "ordinal lookup directories must not duplicate segment identities; observed {bytes} bytes"
        );
    }
}
