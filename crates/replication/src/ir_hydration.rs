//! Versioned semantic-plane selection and bounded client hydration.
//!
//! The semantic crate owns canonical manifest and segment identities. This
//! module binds those identities to the index's currently selected frontier,
//! meters missing byte ranges with replication credits, and admits completed
//! payloads before the semantic cursor advances. It deliberately creates no
//! authority witness and has no publication operation.
//!
//! The locald authority adapter reopens the selected catalog and image manifest
//! and re-reads the frontier for every operation; the client crate supplies the
//! bounded local range path. Callers admit complete canonical catalog and
//! manifest bytes before constructing a cursor. Source objects continue through
//! the existing immutable closure transfer; semantic plane manifests
//! distinguish IR from independently keyed embeddings.

use std::fmt;

use backend_semantic::ir::{
    LanguageProfile, MAX_SEMANTIC_SEGMENT_BYTES, SemanticCoverageState, SemanticDeltaAction,
    SemanticDeltaCursor, SemanticHydrationCoverage, SemanticHydrationCursor,
    SemanticHydrationCursorToken, SemanticInputWitness, SemanticManifestError,
    SemanticManifestRoot, SemanticPlaneCatalog, SemanticPlaneCatalogEntry,
    SemanticPlaneCatalogRoot, SemanticPlaneImageKey, SemanticPlaneKind, SemanticPlaneManifest,
    SemanticPlaneRoot, SemanticPlaneSegment, SemanticRangeRequest, SemanticSegmentId,
    UntrustedSemanticSegmentId,
};
use backend_version::Coverage;

use crate::{ByteRange, ReplicationError, SparseCoverage, TransportLimits};

/// A transport-neutral claim describing the selected index frontier.
///
/// The index adapter constructs this from its current `SelectedFrontier` and
/// re-presents it on every range request and before payload exposure. This is
/// a checked identity claim, not an authority capability: it cannot publish,
/// mint coverage, or replace the index adapter's own frontier admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectedGenerationStamp {
    namespace: [u8; 16],
    profile: LanguageProfile,
    source_coordinate: [u8; 32],
    selection_revision: u64,
    selected_root: [u8; 32],
    closure_id: [u8; 32],
    catalog_root: SemanticPlaneCatalogRoot,
}

impl SelectedGenerationStamp {
    /// Checks and records one exact selected-frontier stamp.
    ///
    /// `selection_revision` is the index authority's monotonic generation or
    /// selection revision. `source_coordinate`, `selected_root`, and
    /// `closure_id` are copied from that same selected frontier. The profile
    /// `catalog_root` commits the complete semantic-image inventory.
    ///
    /// This validates claim shape only. It does not query Turso or establish
    /// freshness; the production adapter must call its authority for each
    /// current stamp supplied to a cursor operation.
    #[allow(clippy::too_many_arguments)]
    pub fn checked(
        namespace: [u8; 16],
        profile: LanguageProfile,
        source_coordinate: [u8; 32],
        selection_revision: u64,
        selected_root: [u8; 32],
        closure_id: [u8; 32],
        catalog_root: SemanticPlaneCatalogRoot,
    ) -> Result<Self, IrHydrationError> {
        if namespace == [0; 16]
            || source_coordinate == [0; 32]
            || selection_revision == 0
            || selected_root == [0; 32]
            || closure_id == [0; 32]
            || catalog_root.as_bytes() == &[0; 32]
        {
            return Err(IrHydrationError::InvalidSelectionStamp);
        }
        Ok(Self {
            namespace,
            profile,
            source_coordinate,
            selection_revision,
            selected_root,
            closure_id,
            catalog_root,
        })
    }

    #[must_use]
    /// Returns the index authority namespace bytes.
    pub const fn namespace(&self) -> &[u8; 16] {
        &self.namespace
    }

    #[must_use]
    /// Returns the selected language profile.
    pub const fn profile(&self) -> LanguageProfile {
        self.profile
    }

    #[must_use]
    /// Returns the selected source coordinate bytes.
    pub const fn source_coordinate(&self) -> &[u8; 32] {
        &self.source_coordinate
    }

    #[must_use]
    /// Returns the monotonic selection revision.
    pub const fn selection_revision(&self) -> u64 {
        self.selection_revision
    }

    #[must_use]
    /// Returns the exact selected logical root bytes.
    pub const fn selected_root(&self) -> &[u8; 32] {
        &self.selected_root
    }

    #[must_use]
    /// Returns the exact selected closure identity bytes.
    pub const fn closure_id(&self) -> &[u8; 32] {
        &self.closure_id
    }

    #[must_use]
    /// Returns the root of the selected semantic image catalog.
    pub const fn catalog_root(&self) -> SemanticPlaneCatalogRoot {
        self.catalog_root
    }
}

/// Live authority reader used at every hydration request, acknowledgement,
/// and payload exposure.
///
/// Production implementations must query the selected frontier on each call.
/// Returning a cached stamp defeats this interface's freshness guarantee.
pub trait SelectedGenerationSource {
    /// Authority read failure.
    type Error: fmt::Display;

    /// Reads the exact currently selected frontier and binds its stamp.
    fn current_selected_generation(&mut self) -> Result<SelectedGenerationStamp, Self::Error>;

    /// Checks exact image membership in a freshly reopened current catalog.
    /// Implementations must re-read authority and bind this answer to the
    /// supplied aggregate stamp; cached membership is not sufficient.
    fn selected_image_is_current(
        &mut self,
        expected_stamp: SelectedGenerationStamp,
        image: SemanticPlaneImageKey,
    ) -> Result<bool, Self::Error>;
}

fn read_current<S: SelectedGenerationSource>(
    source: &mut S,
) -> Result<SelectedGenerationStamp, IrHydrationError> {
    source
        .current_selected_generation()
        .map_err(|error| IrHydrationError::Frontier(error.to_string()))
}

/// Exact selected plane inside one authority-selected semantic generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectedSemanticPlane {
    stamp: SelectedGenerationStamp,
    image: SemanticPlaneImageKey,
    kind: SemanticPlaneKind,
    root: SemanticPlaneRoot,
}

impl SelectedSemanticPlane {
    /// Binds a semantic manifest plane to the index adapter's current stamp.
    ///
    /// # Errors
    ///
    /// Returns a `Stale`, `Missing`, or `Unavailable` terminal when the stamp
    /// does not select this exact manifest, the requested plane is absent, or
    /// the plane/input closure is explicitly unavailable.
    pub fn select<S: SelectedGenerationSource>(
        source: &mut S,
        manifest: &SemanticPlaneManifest,
        image: SemanticPlaneImageKey,
        kind: SemanticPlaneKind,
    ) -> Result<Self, IrHydrationError> {
        let stamp = read_current(source)?;
        if !source
            .selected_image_is_current(stamp, image)
            .map_err(|error| IrHydrationError::Frontier(error.to_string()))?
        {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        Self::select_stamp(stamp, manifest, image, kind)
    }

    fn select_stamp(
        stamp: SelectedGenerationStamp,
        manifest: &SemanticPlaneManifest,
        image: SemanticPlaneImageKey,
        kind: SemanticPlaneKind,
    ) -> Result<Self, IrHydrationError> {
        if image.manifest_root() != manifest.root()
            || image.semantic_generation() != manifest.semantic_generation()
            || stamp.profile != manifest.build().profile()
        {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        let plane = manifest
            .plane(kind)
            .ok_or(IrHydrationError::Terminal(IrHydrationTerminal::Missing))?;
        if is_unavailable(plane.coverage()) || is_unavailable(manifest.input().coverage()) {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Unavailable));
        }
        Ok(Self {
            stamp,
            image,
            kind,
            root: plane.root(),
        })
    }

    #[must_use]
    /// Returns the exact authority-selected generation stamp.
    pub const fn stamp(&self) -> SelectedGenerationStamp {
        self.stamp
    }

    #[must_use]
    /// Returns the exact image tuple admitted by the selected catalog.
    pub const fn image(&self) -> SemanticPlaneImageKey {
        self.image
    }

    #[must_use]
    /// Returns the selected IR or embedding plane kind.
    pub const fn kind(&self) -> SemanticPlaneKind {
        self.kind
    }

    #[must_use]
    /// Returns the selected plane's canonical content root.
    pub const fn root(&self) -> SemanticPlaneRoot {
        self.root
    }

    fn require_current<S: SelectedGenerationSource>(
        self,
        source: &mut S,
    ) -> Result<(), IrHydrationError> {
        let current = read_current(source)?;
        let member = source
            .selected_image_is_current(self.stamp, self.image)
            .map_err(|error| IrHydrationError::Frontier(error.to_string()))?;
        if self.stamp != current || !member {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        Ok(())
    }
}

/// Byte and request-range credits available to one hydration poll.
///
/// One poll emits at most one byte interval. The caller may grant zero credits
/// to pause; the same missing interval will be returned when credits resume.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HydrationCredits {
    range_slots: usize,
    byte_budget: u64,
}

impl HydrationCredits {
    #[must_use]
    /// Creates a per-poll range-slot and byte budget.
    pub const fn new(range_slots: usize, byte_budget: u64) -> Self {
        Self {
            range_slots,
            byte_budget,
        }
    }

    #[must_use]
    /// Returns the range-slot budget for one poll.
    pub const fn range_slots(self) -> usize {
        self.range_slots
    }

    #[must_use]
    /// Returns the byte budget for one poll.
    pub const fn byte_budget(self) -> u64 {
        self.byte_budget
    }
}

/// Sparse byte coverage associated with one untrusted manifest segment claim.
///
/// This wraps the existing receiving-CAS coverage; it does not encode or
/// persist bytes. A caller must restore it from the same CAS checkpoint and
/// bind it to the exact outstanding segment ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SparseSegmentCoverage {
    segment: UntrustedSemanticSegmentId,
    bytes: SparseCoverage,
}

impl SparseSegmentCoverage {
    #[must_use]
    /// Associates existing sparse CAS coverage with one segment claim.
    pub fn new(segment: UntrustedSemanticSegmentId, bytes: SparseCoverage) -> Self {
        Self { segment, bytes }
    }

    #[must_use]
    /// Returns the checked untrusted segment claim associated with this coverage.
    pub const fn segment(&self) -> UntrustedSemanticSegmentId {
        self.segment
    }

    #[must_use]
    /// Returns the sparse byte extents recorded by the existing CAS receiver.
    pub const fn bytes(&self) -> &SparseCoverage {
        &self.bytes
    }
}

/// One logical segment claim and its next bounded byte-range fetch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrHydrationRequest {
    selection: SelectedSemanticPlane,
    segment: SemanticRangeRequest,
    byte_range: Option<ByteRange>,
}

impl IrHydrationRequest {
    #[must_use]
    /// Returns the selection whose manifest and plane define this request.
    pub const fn selection(&self) -> SelectedSemanticPlane {
        self.selection
    }

    #[must_use]
    /// Returns verified segment and range metadata while the selection is current.
    pub fn segment<S: SelectedGenerationSource>(
        &self,
        source: &mut S,
    ) -> Result<SemanticRangeRequest, IrHydrationError> {
        self.selection.require_current(source)?;
        Ok(self.segment)
    }

    /// Missing byte interval to request from the existing CAS/Iroh/Bao path.
    /// `None` means all bytes are already staged locally and need identity
    /// admission before they can count as present.
    #[must_use]
    /// Returns the next byte range while the selection is current.
    pub fn byte_range<S: SelectedGenerationSource>(
        &self,
        source: &mut S,
    ) -> Result<Option<ByteRange>, IrHydrationError> {
        self.selection.require_current(source)?;
        Ok(self.byte_range)
    }
}

/// Result of one bounded hydration poll.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IrHydrationPoll {
    /// One missing range is ready for the caller-owned transfer session.
    Request(IrHydrationRequest),
    /// The logical segment is already sparse-complete locally and awaits a
    /// complete payload read/admission from the CAS.
    VerifyLocal(IrHydrationRequest),
    /// The cursor is waiting for nonzero byte and range credits.
    NoCredits,
    /// No selected segment remains missing from the verified-have set.
    Exhausted,
}

/// Durable semantic cursor state. Persist this typed selection alongside the
/// existing CAS checkpoint and the semantic cursor token's canonical bytes;
/// partial payload extents stay in the CAS checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrHydrationCheckpoint {
    selection: SelectedSemanticPlane,
    cursor: SemanticHydrationCursorToken,
}

impl IrHydrationCheckpoint {
    /// Rebinds the persisted authority stamp and semantic token after process
    /// restart. The exact prior stamp must match a fresh index-authority read,
    /// including its monotonic selection revision, before the token can resume.
    pub fn restore<S: SelectedGenerationSource>(
        manifest: &SemanticPlaneManifest,
        source: &mut S,
        persisted_stamp: SelectedGenerationStamp,
        image: SemanticPlaneImageKey,
        kind: SemanticPlaneKind,
        cursor: SemanticHydrationCursorToken,
    ) -> Result<Self, IrHydrationError> {
        let current = read_current(source)?;
        if current != persisted_stamp {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        if !source
            .selected_image_is_current(current, image)
            .map_err(|error| IrHydrationError::Frontier(error.to_string()))?
        {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        let selection = SelectedSemanticPlane::select_stamp(current, manifest, image, kind)?;
        if cursor.manifest_root() != image.manifest_root() || cursor.plane_filter() != Some(kind) {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        Ok(Self { selection, cursor })
    }

    #[must_use]
    /// Returns the authority-selected plane bound to this checkpoint.
    pub const fn selection(&self) -> SelectedSemanticPlane {
        self.selection
    }

    #[must_use]
    /// Returns the canonical semantic cursor token.
    pub const fn cursor(&self) -> SemanticHydrationCursorToken {
        self.cursor
    }

    /// Returns the semantic layer's canonical cursor encoding. Selection
    /// stamp and sparse extents remain in their existing index/CAS stores.
    pub fn encode_cursor(&self) -> Result<Vec<u8>, SemanticManifestError> {
        self.cursor.encode()
    }
}

/// Terminal/progress state derived from admitted metadata and verified IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrHydrationTerminal {
    /// At least one selected segment still needs verification or transfer.
    Pending,
    /// Every segment and its input/output coverage are authority-admitted.
    Complete,
    /// All segment IDs are present, but reopened claims do not authorize reuse.
    Unverified,
    /// The requested plane or generation does not exist in the selected root.
    Missing,
    /// The selected manifest or current index frontier moved.
    Stale,
    /// Input or output closure is unavailable/unsupported.
    Unavailable,
}

/// Payload that passed semantic identity verification but is not yet durable.
/// It cannot advance hydration coverage or be exposed as a completed segment.
#[derive(Debug)]
pub struct PendingSemanticSegment<'payload> {
    selection: SelectedSemanticPlane,
    segment: SemanticSegmentId,
    payload: &'payload [u8],
}

impl PendingSemanticSegment<'_> {
    #[must_use]
    /// Returns the selected plane that defines this segment.
    pub const fn selection(&self) -> SelectedSemanticPlane {
        self.selection
    }

    #[must_use]
    /// Returns the content identity verified against the manifest claim.
    pub const fn id(&self) -> SemanticSegmentId {
        self.segment
    }

    #[must_use]
    /// Returns the verified payload's exact byte length.
    pub fn payload_len(&self) -> usize {
        self.payload.len()
    }
}

/// Storage adapter for one complete semantic segment.
///
/// Implementations must durably commit the exact bytes under the checked
/// segment identity, then read/admit those bytes back from the CAS before
/// returning. A successful return is the storage receipt boundary; callers
/// cannot advance the semantic cursor with a transient hash check alone.
pub trait DurableSemanticSegmentStore {
    /// Storage-specific failure reported before any hydration acknowledgement.
    type Error: fmt::Display;

    /// Fsyncs one checked segment and returns the bytes read back by identity.
    ///
    /// The returned payload is checked against the semantic identity again by
    /// this module before the cursor acknowledges the segment.
    fn commit_and_read(
        &mut self,
        selection: SelectedSemanticPlane,
        segment: SemanticSegmentId,
        payload: &[u8],
        admit: &mut dyn FnMut(&[u8]) -> Result<(), ReplicationError>,
    ) -> Result<Box<[u8]>, Self::Error>;
}

/// Opaque evidence that exact selected semantic bytes passed durable CAS
/// commit and read-back admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DurableSemanticSegmentReceipt {
    selection: SelectedSemanticPlane,
    segment: SemanticSegmentId,
    byte_length: u64,
}

impl DurableSemanticSegmentReceipt {
    fn checked(
        selection: SelectedSemanticPlane,
        segment: SemanticSegmentId,
        byte_length: u64,
    ) -> Self {
        Self {
            selection,
            segment,
            byte_length,
        }
    }

    #[must_use]
    /// Returns the selected plane whose bytes were durably admitted.
    pub const fn selection(&self) -> SelectedSemanticPlane {
        self.selection
    }

    #[must_use]
    /// Returns the checked segment identity admitted to the CAS.
    pub const fn segment(&self) -> SemanticSegmentId {
        self.segment
    }

    #[must_use]
    /// Returns the exact persisted payload size.
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }
}

/// Checked segment bytes read back from durable CAS and bound to one exact
/// selected plane. Payload access rechecks the index head stamp.
#[derive(Debug)]
pub struct VerifiedSemanticSegment {
    receipt: DurableSemanticSegmentReceipt,
    payload: Box<[u8]>,
}

impl VerifiedSemanticSegment {
    #[must_use]
    /// Returns the durable CAS receipt for these selected bytes.
    pub const fn receipt(&self) -> DurableSemanticSegmentReceipt {
        self.receipt
    }

    #[must_use]
    /// Returns the selected plane whose bytes were durably admitted.
    pub const fn selection(&self) -> SelectedSemanticPlane {
        self.receipt.selection
    }

    #[must_use]
    /// Returns the checked segment identity admitted to durable CAS.
    pub const fn id(&self) -> SemanticSegmentId {
        self.receipt.segment
    }

    /// Exposes admitted bytes only while the same selected frontier remains current.
    pub fn payload<S: SelectedGenerationSource>(
        &self,
        source: &mut S,
    ) -> Result<&[u8], IrHydrationError> {
        self.receipt.selection.require_current(source)?;
        Ok(&self.payload)
    }
}

/// One selected-plane cursor over the semantic layer's canonical range plan.
pub struct IrHydrationCursor<'manifest, 'have> {
    manifest: &'manifest SemanticPlaneManifest,
    selection: SelectedSemanticPlane,
    cursor: SemanticHydrationCursor<'manifest, 'have>,
    limits: TransportLimits,
    pending: Option<SemanticRangeRequest>,
}

impl<'manifest, 'have> IrHydrationCursor<'manifest, 'have> {
    /// Starts fresh hydration after binding the manifest to an exact current selection.
    pub fn new<S: SelectedGenerationSource>(
        manifest: &'manifest SemanticPlaneManifest,
        source: &mut S,
        plane: SemanticPlaneKind,
        have_ids: &'have [SemanticSegmentId],
        limits: TransportLimits,
    ) -> Result<Self, IrHydrationError> {
        Self::new_for_image(
            manifest,
            source,
            SemanticPlaneImageKey::from_manifest(0, manifest),
            plane,
            have_ids,
            limits,
        )
    }

    /// Starts fresh hydration for an exact image member of the selected catalog.
    pub fn new_for_image<S: SelectedGenerationSource>(
        manifest: &'manifest SemanticPlaneManifest,
        source: &mut S,
        image: SemanticPlaneImageKey,
        plane: SemanticPlaneKind,
        have_ids: &'have [SemanticSegmentId],
        limits: TransportLimits,
    ) -> Result<Self, IrHydrationError> {
        let limits = limits.validate()?;
        let selection = SelectedSemanticPlane::select(source, manifest, image, plane)?;
        let cursor = manifest.hydration_cursor(manifest.root(), Some(plane), have_ids)?;
        Ok(Self {
            manifest,
            selection,
            cursor,
            limits,
            pending: None,
        })
    }

    /// Restores a semantic cursor against the exact selected stamp, manifest,
    /// plane root, and CAS-verified local-have set.
    pub fn resume<S: SelectedGenerationSource>(
        manifest: &'manifest SemanticPlaneManifest,
        checkpoint: IrHydrationCheckpoint,
        source: &mut S,
        have_ids: &'have [SemanticSegmentId],
        limits: TransportLimits,
    ) -> Result<Self, IrHydrationError> {
        let limits = limits.validate()?;
        let current = read_current(source)?;
        if checkpoint.selection.stamp != current
            || !source
                .selected_image_is_current(current, checkpoint.selection.image)
                .map_err(|error| IrHydrationError::Frontier(error.to_string()))?
        {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        let selection = SelectedSemanticPlane::select_stamp(
            current,
            manifest,
            checkpoint.selection.image,
            checkpoint.selection.kind,
        )?;
        if selection != checkpoint.selection {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        let cursor = match SemanticHydrationCursor::resume(manifest, checkpoint.cursor, have_ids) {
            Ok(cursor) => cursor,
            Err(SemanticManifestError::StaleManifest { .. }) => {
                return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
            }
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            manifest,
            selection,
            cursor,
            limits,
            pending: None,
        })
    }

    /// Polls one bounded byte range, repeating a dropped request until the
    /// CAS reports its bytes in `partial` or the complete segment is admitted.
    pub fn next_request<S: SelectedGenerationSource>(
        &mut self,
        source: &mut S,
        partial: Option<&SparseSegmentCoverage>,
        credits: HydrationCredits,
    ) -> Result<IrHydrationPoll, IrHydrationError> {
        self.selection.require_current(source)?;
        if self.terminal_unchecked() == IrHydrationTerminal::Unavailable {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Unavailable));
        }
        let Some(segment) = self.cursor.next_request() else {
            self.pending = None;
            return Ok(IrHydrationPoll::Exhausted);
        };
        if segment.manifest_root != self.selection.image.manifest_root()
            || segment.plane != self.selection.kind
        {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        if self.pending.is_some_and(|pending| pending != segment) {
            return Err(IrHydrationError::OutOfOrderSegment);
        }
        self.pending = Some(segment);
        if segment.byte_length > self.limits.max_object {
            return Err(ReplicationError::ObjectTooLarge.into());
        }
        if segment.byte_length > MAX_SEMANTIC_SEGMENT_BYTES as u64 {
            return Err(IrHydrationError::Semantic(
                SemanticManifestError::SegmentBytes {
                    observed: segment.byte_length,
                    maximum: MAX_SEMANTIC_SEGMENT_BYTES as u64,
                },
            ));
        }

        let empty_coverage = SparseCoverage::new(self.limits.max_ranges)?;
        let coverage = match partial {
            Some(partial) if partial.segment != segment.segment_id => {
                return Err(IrHydrationError::OutOfOrderSegment);
            }
            Some(partial) => &partial.bytes,
            None => &empty_coverage,
        };
        if coverage.ranges().len() > self.limits.max_ranges {
            return Err(ReplicationError::CoverageLimit.into());
        }
        let missing = coverage.missing(segment.byte_length, self.limits.max_ranges)?;
        if missing.is_empty() {
            return Ok(IrHydrationPoll::VerifyLocal(IrHydrationRequest {
                selection: self.selection,
                segment,
                byte_range: None,
            }));
        }
        let range_slots = credits.range_slots.min(self.limits.max_ranges);
        let max_chunk =
            u64::try_from(self.limits.max_chunk).map_err(|_| ReplicationError::Overflow)?;
        let byte_budget = credits.byte_budget.min(max_chunk);
        if range_slots == 0 || byte_budget == 0 {
            return Ok(IrHydrationPoll::NoCredits);
        }
        let first = missing
            .first()
            .copied()
            .ok_or(IrHydrationError::OutOfOrderSegment)?;
        let requested = ByteRange::new(first.start, first.len.min(byte_budget))?;
        Ok(IrHydrationPoll::Request(IrHydrationRequest {
            selection: self.selection,
            segment,
            byte_range: Some(requested),
        }))
    }

    /// Checks one complete payload against the manifest without advancing the
    /// cursor or exposing it as a completed segment.
    pub fn verify_payload<'payload, S: SelectedGenerationSource>(
        &self,
        source: &mut S,
        request: &IrHydrationRequest,
        payload: &'payload [u8],
    ) -> Result<PendingSemanticSegment<'payload>, IrHydrationError> {
        self.selection.require_current(source)?;
        if request.selection != self.selection {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        let Some(pending) = self.pending else {
            return Err(IrHydrationError::NoPendingSegment);
        };
        if request.segment != pending {
            return Err(IrHydrationError::OutOfOrderSegment);
        }
        let segment = self.segment_for(pending)?;
        let admitted_id = segment.admit(self.selection.kind, payload)?;
        Ok(PendingSemanticSegment {
            selection: self.selection,
            segment: admitted_id,
            payload,
        })
    }

    /// Durably commits a verified payload, verifies the CAS read-back, and
    /// only then advances the semantic cursor.
    pub fn commit_admitted_segment<A: SelectedGenerationSource, S: DurableSemanticSegmentStore>(
        &mut self,
        source: &mut A,
        request: &IrHydrationRequest,
        pending_payload: PendingSemanticSegment<'_>,
        store: &mut S,
    ) -> Result<VerifiedSemanticSegment, IrHydrationError> {
        self.selection.require_current(source)?;
        if request.selection != self.selection || pending_payload.selection != self.selection {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        let Some(pending) = self.pending else {
            return Err(IrHydrationError::NoPendingSegment);
        };
        if request.segment != pending {
            return Err(IrHydrationError::OutOfOrderSegment);
        }
        if pending_payload.segment
            != self
                .segment_for(pending)?
                .admit(self.selection.kind, pending_payload.payload)?
        {
            return Err(IrHydrationError::OutOfOrderSegment);
        }
        let payload = store
            .commit_and_read(
                self.selection,
                pending_payload.segment,
                pending_payload.payload,
                &mut |bytes| {
                    let admitted = self
                        .segment_for(pending)
                        .map_err(|_| ReplicationError::IdentityMismatch)?
                        .admit(self.selection.kind, bytes)
                        .map_err(|_| ReplicationError::IdentityMismatch)?;
                    if admitted == pending_payload.segment {
                        Ok(())
                    } else {
                        Err(ReplicationError::IdentityMismatch)
                    }
                },
            )
            .map_err(|error| IrHydrationError::Storage(error.to_string()))?;
        let segment = self.segment_for(pending)?;
        let admitted_id = segment.admit(self.selection.kind, &payload)?;
        if admitted_id != pending_payload.segment {
            return Err(IrHydrationError::OutOfOrderSegment);
        }
        let byte_length = u64::try_from(payload.len()).map_err(|_| ReplicationError::Overflow)?;
        // CAS I/O can take long enough for another generation to become the
        // selected head. Re-read immediately before advancing coverage.
        self.selection.require_current(source)?;
        self.cursor.acknowledge(admitted_id)?;
        self.pending = None;
        let receipt =
            DurableSemanticSegmentReceipt::checked(self.selection, admitted_id, byte_length);
        Ok(VerifiedSemanticSegment { receipt, payload })
    }

    fn segment_for(
        &self,
        pending: SemanticRangeRequest,
    ) -> Result<&SemanticPlaneSegment, IrHydrationError> {
        self.manifest
            .plane(self.selection.kind)
            .ok_or(IrHydrationError::Terminal(IrHydrationTerminal::Missing))?
            .segments()
            .iter()
            .find(|candidate| candidate.id_claim() == pending.segment_id)
            .ok_or(IrHydrationError::Terminal(IrHydrationTerminal::Stale))
    }

    /// Captures a root-bound cursor token for durable resume while this exact
    /// frontier remains selected.
    pub fn checkpoint<S: SelectedGenerationSource>(
        &self,
        source: &mut S,
    ) -> Result<IrHydrationCheckpoint, IrHydrationError> {
        self.selection.require_current(source)?;
        Ok(IrHydrationCheckpoint {
            selection: self.selection,
            cursor: self.cursor.checkpoint(),
        })
    }

    /// Returns semantic-plane coverage only while this frontier remains
    /// selected.
    pub fn coverage<S: SelectedGenerationSource>(
        &self,
        source: &mut S,
    ) -> Result<SemanticHydrationCoverage, IrHydrationError> {
        self.selection.require_current(source)?;
        Ok(self.cursor.coverage())
    }

    /// Returns hydration status only while this frontier remains selected.
    pub fn terminal<S: SelectedGenerationSource>(
        &self,
        source: &mut S,
    ) -> Result<IrHydrationTerminal, IrHydrationError> {
        self.selection.require_current(source)?;
        Ok(self.terminal_unchecked())
    }

    fn terminal_unchecked(&self) -> IrHydrationTerminal {
        let coverage = self.cursor.coverage();
        if is_unavailable(coverage.plane) || is_unavailable(coverage.input) {
            return IrHydrationTerminal::Unavailable;
        }
        if coverage.missing_segments != 0 {
            return IrHydrationTerminal::Pending;
        }
        if self.manifest.claims_admitted()
            && coverage.is_reusable()
            && coverage.segment_inputs_authorized
        {
            IrHydrationTerminal::Complete
        } else {
            IrHydrationTerminal::Unverified
        }
    }
}

/// Whether local compiler output can be considered for reuse before walking
/// the semantic delta. Exact build equality includes target platform,
/// toolchain, recipe, environment, profile, and stage; exact admitted input
/// equality includes the positive and negative read-frontier witness.
///
/// The semantic delta cursor must still admit each individual output segment
/// and its per-segment input witness. This function grants no publish right.
pub fn local_build_reuse_eligible(
    base: &SemanticPlaneManifest,
    target: &SemanticPlaneManifest,
    expected_base_root: SemanticManifestRoot,
) -> Result<bool, IrHydrationError> {
    if base.root() != expected_base_root {
        return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
    }
    Ok(base.claims_admitted()
        && target.claims_admitted()
        && base.build() == target.build()
        && same_admitted_input(&base.input(), &target.input()))
}

/// Current-selection guarded delta cursor. Each action is returned only while
/// both the base and target authority stamps still match their selected roots.
pub struct SelectedSemanticDeltaCursor<'base, 'target> {
    base_stamp: SelectedGenerationStamp,
    base_image: SemanticPlaneImageKey,
    target_stamp: SelectedGenerationStamp,
    target_image: SemanticPlaneImageKey,
    cursor: SemanticDeltaCursor<'base, 'target>,
}

/// Delta action bound to the exact selected base and target frontiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectedSemanticDeltaAction {
    base_stamp: SelectedGenerationStamp,
    base_image: SemanticPlaneImageKey,
    target_stamp: SelectedGenerationStamp,
    target_image: SemanticPlaneImageKey,
    action: SemanticDeltaAction,
}

impl SelectedSemanticDeltaAction {
    #[must_use]
    /// Returns the exact selected base stamp bound to this action.
    pub const fn base_stamp(&self) -> SelectedGenerationStamp {
        self.base_stamp
    }

    #[must_use]
    /// Returns the exact selected target stamp bound to this action.
    pub const fn target_stamp(&self) -> SelectedGenerationStamp {
        self.target_stamp
    }

    #[must_use]
    /// Returns the action only while both selected frontiers remain current.
    pub fn action<A: SelectedGenerationSource, B: SelectedGenerationSource>(
        &self,
        base_source: &mut A,
        target_source: &mut B,
    ) -> Result<SemanticDeltaAction, IrHydrationError> {
        if self.base_stamp != read_current(base_source)?
            || self.target_stamp != read_current(target_source)?
            || !base_source
                .selected_image_is_current(self.base_stamp, self.base_image)
                .map_err(|error| IrHydrationError::Frontier(error.to_string()))?
            || !target_source
                .selected_image_is_current(self.target_stamp, self.target_image)
                .map_err(|error| IrHydrationError::Frontier(error.to_string()))?
        {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        Ok(self.action)
    }
}

impl<'base, 'target> SelectedSemanticDeltaCursor<'base, 'target> {
    /// Starts delta planning only for the exact selected base and target roots.
    pub fn new<A: SelectedGenerationSource, B: SelectedGenerationSource>(
        base: &'base SemanticPlaneManifest,
        base_source: &mut A,
        target: &'target SemanticPlaneManifest,
        target_source: &mut B,
        expected_base_root: SemanticManifestRoot,
    ) -> Result<Self, IrHydrationError> {
        Self::new_for_images(
            base,
            SemanticPlaneImageKey::from_manifest(0, base),
            base_source,
            target,
            SemanticPlaneImageKey::from_manifest(0, target),
            target_source,
            expected_base_root,
        )
    }

    /// Starts delta planning for two exact image members of their selected catalogs.
    pub fn new_for_images<A: SelectedGenerationSource, B: SelectedGenerationSource>(
        base: &'base SemanticPlaneManifest,
        base_image: SemanticPlaneImageKey,
        base_source: &mut A,
        target: &'target SemanticPlaneManifest,
        target_image: SemanticPlaneImageKey,
        target_source: &mut B,
        expected_base_root: SemanticManifestRoot,
    ) -> Result<Self, IrHydrationError> {
        let base_stamp = read_current(base_source)?;
        let target_stamp = read_current(target_source)?;
        if base_image.manifest_root() != expected_base_root
            || base_image.manifest_root() != base.root()
            || base_image.semantic_generation() != base.semantic_generation()
            || target_image.manifest_root() != target.root()
            || target_image.semantic_generation() != target.semantic_generation()
            || base_stamp.profile != base.build().profile()
            || target_stamp.profile != target.build().profile()
        {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        if !base_source
            .selected_image_is_current(base_stamp, base_image)
            .map_err(|error| IrHydrationError::Frontier(error.to_string()))?
            || !target_source
                .selected_image_is_current(target_stamp, target_image)
                .map_err(|error| IrHydrationError::Frontier(error.to_string()))?
        {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        let cursor = SemanticDeltaCursor::new(base, target, expected_base_root)?;
        Ok(Self {
            base_stamp,
            base_image,
            target_stamp,
            target_image,
            cursor,
        })
    }

    /// Returns one checked delta operation if both heads remain selected.
    pub fn next_action<A: SelectedGenerationSource, B: SelectedGenerationSource>(
        &mut self,
        base_source: &mut A,
        target_source: &mut B,
    ) -> Result<Option<SelectedSemanticDeltaAction>, IrHydrationError> {
        if read_current(base_source)? != self.base_stamp
            || read_current(target_source)? != self.target_stamp
            || !base_source
                .selected_image_is_current(self.base_stamp, self.base_image)
                .map_err(|error| IrHydrationError::Frontier(error.to_string()))?
            || !target_source
                .selected_image_is_current(self.target_stamp, self.target_image)
                .map_err(|error| IrHydrationError::Frontier(error.to_string()))?
        {
            return Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale));
        }
        Ok(self
            .cursor
            .next_action()
            .map(|action| SelectedSemanticDeltaAction {
                base_stamp: self.base_stamp,
                base_image: self.base_image,
                target_stamp: self.target_stamp,
                target_image: self.target_image,
                action,
            }))
    }
}

/// Errors from selection binding, bounded transfer planning, or byte admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IrHydrationError {
    /// A terminal state prevents the requested operation.
    Terminal(IrHydrationTerminal),
    /// The current stamp omitted or contradicted required authority identity.
    InvalidSelectionStamp,
    /// The live index authority could not be queried for its selected frontier.
    Frontier(String),
    /// The caller tried to acknowledge a segment other than the active one.
    OutOfOrderSegment,
    /// No segment request is awaiting complete payload admission.
    NoPendingSegment,
    /// The semantic manifest rejected its root, metadata, or payload claim.
    Semantic(SemanticManifestError),
    /// A sparse range or transport budget failed bounded replication validation.
    Replication(ReplicationError),
    /// Durable CAS write or read-back failed before cursor acknowledgement.
    Storage(String),
}

impl From<SemanticManifestError> for IrHydrationError {
    fn from(value: SemanticManifestError) -> Self {
        Self::Semantic(value)
    }
}

impl From<ReplicationError> for IrHydrationError {
    fn from(value: ReplicationError) -> Self {
        Self::Replication(value)
    }
}

impl fmt::Display for IrHydrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "IR hydration error: {self:?}")
    }
}

impl std::error::Error for IrHydrationError {}

fn is_unavailable(coverage: SemanticCoverageState) -> bool {
    matches!(
        coverage.state(),
        Coverage::Unavailable | Coverage::Unsupported
    )
}

fn same_admitted_input(left: &SemanticInputWitness, right: &SemanticInputWitness) -> bool {
    left.same_admitted_frontier(right)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::collections::VecDeque;
    use std::fs::{self, File};
    use std::io::Write;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::{
        FileSemanticRangeStore, SemanticRangeChunk, SemanticRangeClientCheckpoint,
        SemanticRangeClientProgress, SemanticRangeGet, SemanticTargetKey, accept_semantic_range,
    };
    use backend_semantic::ir::{
        GenerationId, LanguageProfile, RustEdition, SemanticBuildIdentity, SemanticIrPlane,
        SemanticPlane, SemanticPlaneCoverageScope, SemanticPlaneManifest, SemanticPlaneSegment,
    };
    use backend_semantic::vocabulary::Stage;
    use backend_store::{FileStore, GcLimits, GcRoots, TypedObject, UntrustedObjectId};
    use backend_version::{
        AuthorityScopeClaim, Coverage, CoverageAdmissionError, CoverageWitness, ObjectKey,
        ObjectVersion, ProducerObservationClaims, ProducerObservationVerifier, Schema, ScopeRoot,
        UntrustedProducerObservation, admit_complete_scope, admit_producer_observation,
    };

    use super::*;

    fn build(platform: u8) -> SemanticBuildIdentity {
        SemanticBuildIdentity::new(
            [1; 32],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
            [platform; 32],
        )
    }

    fn generation(payloads: &[&[u8]]) -> GenerationId {
        let mut canonical = b"replication.ir-hydration.fixture.v1\0".to_vec();
        canonical.extend_from_slice(
            &u64::try_from(payloads.len())
                .expect("small fixture count")
                .to_be_bytes(),
        );
        for payload in payloads {
            canonical.extend_from_slice(
                &u64::try_from(payload.len())
                    .expect("small fixture payload")
                    .to_be_bytes(),
            );
            canonical.extend_from_slice(payload);
        }
        GenerationId::from_canonical_bytes(&canonical)
    }

    fn input(identity: u8) -> SemanticInputWitness {
        SemanticInputWitness::claimed([identity; 32], ScopeRoot::from_bytes([9; 32]))
    }

    struct TestAuthority;

    impl Schema for TestAuthority {
        const DOMAIN: u8 = 0x53;
        const TYPE: u16 = 0xfffd;
        type Value = [u8; 32];

        fn encode(value: &Self::Value, out: &mut Vec<u8>) {
            out.extend_from_slice(value);
        }
    }

    struct TestVerifier;

    impl ProducerObservationVerifier for TestVerifier {
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

    fn complete_witness<T: Schema>(version: ObjectVersion<T>) -> CoverageWitness {
        let claim = AuthorityScopeClaim::from_object_version(version);
        let scope = claim.scope_root();
        let admitted = admit_producer_observation(
            UntrustedProducerObservation::new([6; 32], scope, [7; 32], scope.as_bytes().to_vec()),
            &TestVerifier,
        )
        .expect("producer witness");
        CoverageWitness::Complete(
            admit_complete_scope(claim, admitted).expect("exact scope coverage"),
        )
    }

    fn admitted_input(identity: u8) -> SemanticInputWitness {
        let version = ObjectVersion::<TestAuthority>::from_value(&[8; 32]);
        SemanticInputWitness::admitted(
            [identity; 32],
            ScopeRoot::from_bytes(version.to_bytes()),
            complete_witness(version),
        )
        .expect("input witness")
    }

    fn manifest(payloads: &[&[u8]], coverage: Coverage, platform: u8) -> SemanticPlaneManifest {
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let segments = payloads
            .iter()
            .enumerate()
            .map(|(index, payload)| {
                let key = [u8::try_from(index + 1).expect("small fixture index"); 32];
                SemanticPlaneSegment::from_payload(kind, key, key, 1, payload)
                    .expect("well formed segment")
            })
            .collect();
        let plane = SemanticPlane::claimed(kind, segments, coverage).expect("plane");
        SemanticPlaneManifest::new(generation(payloads), build(platform), input(7), vec![plane])
            .expect("manifest")
    }

    fn admitted_manifest(
        payload: &[u8],
        input_identity: u8,
        platform: u8,
    ) -> SemanticPlaneManifest {
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let input = admitted_input(input_identity);
        let segment = SemanticPlaneSegment::from_payload_with_witness(
            kind, [1; 32], [1; 32], 1, payload, input,
        )
        .expect("admitted segment bytes");
        let claimed =
            SemanticPlane::claimed(kind, vec![segment], Coverage::Complete).expect("claimed plane");
        let plane_version =
            ObjectVersion::<SemanticPlaneCoverageScope>::from_value(&claimed.root());
        let plane = SemanticPlane::admitted(
            kind,
            claimed.segments().to_vec(),
            complete_witness(plane_version),
        )
        .expect("admitted plane");
        SemanticPlaneManifest::new(generation(&[payload]), build(platform), input, vec![plane])
            .expect("admitted manifest")
    }

    fn stamp(manifest: &SemanticPlaneManifest, revision: u64) -> SelectedGenerationStamp {
        catalog_stamp(&[(0, manifest)], revision)
    }

    fn catalog_stamp(
        manifests: &[(u32, &SemanticPlaneManifest)],
        revision: u64,
    ) -> SelectedGenerationStamp {
        let entries = manifests
            .iter()
            .map(|(ordinal, manifest)| {
                let image = SemanticPlaneImageKey::from_manifest(*ordinal, manifest);
                let manifest_length =
                    u32::try_from(manifest.encode().expect("manifest encoding").len())
                        .expect("small catalog manifest length");
                SemanticPlaneCatalogEntry::new(image, manifest_length).expect("catalog entry")
            })
            .collect();
        let catalog = SemanticPlaneCatalog::new(entries).expect("catalog").root();
        SelectedGenerationStamp::checked(
            [1; 16],
            manifests[0].1.build().profile(),
            [2; 32],
            revision,
            [4; 32],
            [5; 32],
            catalog,
        )
        .expect("selected frontier stamp")
    }

    struct TestSelectionSource {
        current: SelectedGenerationStamp,
        next: VecDeque<SelectedGenerationStamp>,
        members: Option<Vec<SemanticPlaneImageKey>>,
    }

    impl TestSelectionSource {
        fn new(stamp: SelectedGenerationStamp) -> Self {
            Self {
                current: stamp,
                next: VecDeque::new(),
                members: None,
            }
        }

        fn with_images(
            stamp: SelectedGenerationStamp,
            images: impl IntoIterator<Item = SemanticPlaneImageKey>,
        ) -> Self {
            Self {
                current: stamp,
                next: VecDeque::new(),
                members: Some(images.into_iter().collect()),
            }
        }

        fn scripted(stamps: impl IntoIterator<Item = SelectedGenerationStamp>) -> Self {
            let mut stamps = stamps.into_iter();
            let current = stamps.next().expect("script has initial stamp");
            Self {
                current,
                next: stamps.collect(),
                members: None,
            }
        }
    }

    impl SelectedGenerationSource for TestSelectionSource {
        type Error = &'static str;

        fn current_selected_generation(&mut self) -> Result<SelectedGenerationStamp, Self::Error> {
            if let Some(next) = self.next.pop_front() {
                self.current = next;
            }
            Ok(self.current)
        }

        fn selected_image_is_current(
            &mut self,
            expected_stamp: SelectedGenerationStamp,
            image: SemanticPlaneImageKey,
        ) -> Result<bool, Self::Error> {
            Ok(self.current == expected_stamp
                && self
                    .members
                    .as_ref()
                    .is_none_or(|members| members.contains(&image)))
        }
    }

    fn limits() -> TransportLimits {
        TransportLimits {
            max_frame: 64,
            max_chunk: 4,
            ..TransportLimits::default()
        }
    }

    fn range_store_limits() -> TransportLimits {
        TransportLimits {
            max_frame: 32 * 1024,
            max_chunk: 16 * 1024,
            ..TransportLimits::default()
        }
    }

    fn independent_fixture_payload() -> Vec<u8> {
        (0..20_000)
            .map(|index| u8::try_from((index * 37 + index / 17 + 13) % 251).expect("byte"))
            .collect()
    }

    fn fixture_target(profile: LanguageProfile) -> SemanticTargetKey {
        SemanticTargetKey::new(
            "pkg:cargo/tentpole-app@1.2.3",
            "pkg:cargo/tentpole-app@1.2.3",
            profile,
        )
        .expect("fixture target")
    }

    fn empty_coverage() -> SparseCoverage {
        SparseCoverage::new(8).expect("coverage budget")
    }

    fn first_request(
        cursor: &mut IrHydrationCursor<'_, '_>,
        stamp: SelectedGenerationStamp,
    ) -> IrHydrationRequest {
        let mut source = TestSelectionSource::new(stamp);
        match cursor
            .next_request(&mut source, None, HydrationCredits::new(1, 4))
            .expect("request")
        {
            IrHydrationPoll::Request(request) => request,
            other => panic!("expected range request, got {other:?}"),
        }
    }

    fn request_segment(
        request: &IrHydrationRequest,
        stamp: SelectedGenerationStamp,
    ) -> SemanticRangeRequest {
        request
            .segment(&mut TestSelectionSource::new(stamp))
            .expect("current segment")
    }

    fn request_range(
        request: &IrHydrationRequest,
        stamp: SelectedGenerationStamp,
    ) -> Option<ByteRange> {
        request
            .byte_range(&mut TestSelectionSource::new(stamp))
            .expect("current range")
    }

    fn poll(
        cursor: &mut IrHydrationCursor<'_, '_>,
        stamp: SelectedGenerationStamp,
        partial: Option<&SparseSegmentCoverage>,
        credits: HydrationCredits,
    ) -> Result<IrHydrationPoll, IrHydrationError> {
        cursor.next_request(&mut TestSelectionSource::new(stamp), partial, credits)
    }

    fn new_cursor<'manifest, 'have>(
        manifest: &'manifest SemanticPlaneManifest,
        stamp: SelectedGenerationStamp,
        kind: SemanticPlaneKind,
        have: &'have [SemanticSegmentId],
        limits: TransportLimits,
    ) -> Result<IrHydrationCursor<'manifest, 'have>, IrHydrationError> {
        let mut source = TestSelectionSource::new(stamp);
        IrHydrationCursor::new(manifest, &mut source, kind, have, limits)
    }

    fn resume_cursor<'manifest, 'have>(
        manifest: &'manifest SemanticPlaneManifest,
        checkpoint: IrHydrationCheckpoint,
        stamp: SelectedGenerationStamp,
        have: &'have [SemanticSegmentId],
        limits: TransportLimits,
    ) -> Result<IrHydrationCursor<'manifest, 'have>, IrHydrationError> {
        let mut source = TestSelectionSource::new(stamp);
        IrHydrationCursor::resume(manifest, checkpoint, &mut source, have, limits)
    }

    fn restore_checkpoint(
        manifest: &SemanticPlaneManifest,
        stamp: SelectedGenerationStamp,
        kind: SemanticPlaneKind,
        token: SemanticHydrationCursorToken,
    ) -> Result<IrHydrationCheckpoint, IrHydrationError> {
        let mut source = TestSelectionSource::new(stamp);
        IrHydrationCheckpoint::restore(
            manifest,
            &mut source,
            stamp,
            SemanticPlaneImageKey::from_manifest(0, manifest),
            kind,
            token,
        )
    }

    fn verify_payload<'payload>(
        cursor: &IrHydrationCursor<'_, '_>,
        stamp: SelectedGenerationStamp,
        request: &IrHydrationRequest,
        payload: &'payload [u8],
    ) -> Result<PendingSemanticSegment<'payload>, IrHydrationError> {
        cursor.verify_payload(&mut TestSelectionSource::new(stamp), request, payload)
    }

    fn commit_pending<S: DurableSemanticSegmentStore>(
        cursor: &mut IrHydrationCursor<'_, '_>,
        stamp: SelectedGenerationStamp,
        request: &IrHydrationRequest,
        pending: PendingSemanticSegment<'_>,
        store: &mut S,
    ) -> Result<VerifiedSemanticSegment, IrHydrationError> {
        cursor.commit_admitted_segment(
            &mut TestSelectionSource::new(stamp),
            request,
            pending,
            store,
        )
    }

    fn expose_payload<'segment>(
        segment: &'segment VerifiedSemanticSegment,
        stamp: SelectedGenerationStamp,
    ) -> Result<&'segment [u8], IrHydrationError> {
        segment.payload(&mut TestSelectionSource::new(stamp))
    }

    fn cursor_checkpoint(
        cursor: &IrHydrationCursor<'_, '_>,
        stamp: SelectedGenerationStamp,
    ) -> IrHydrationCheckpoint {
        cursor
            .checkpoint(&mut TestSelectionSource::new(stamp))
            .expect("current selected checkpoint")
    }

    fn cursor_coverage(
        cursor: &IrHydrationCursor<'_, '_>,
        stamp: SelectedGenerationStamp,
    ) -> SemanticHydrationCoverage {
        cursor
            .coverage(&mut TestSelectionSource::new(stamp))
            .expect("current selected coverage")
    }

    fn cursor_terminal(
        cursor: &IrHydrationCursor<'_, '_>,
        stamp: SelectedGenerationStamp,
    ) -> IrHydrationTerminal {
        cursor
            .terminal(&mut TestSelectionSource::new(stamp))
            .expect("current selected hydration terminal")
    }

    #[derive(Default)]
    struct TestSegmentStore {
        fail_next: bool,
        corrupt_readback: bool,
        payloads: HashMap<SemanticSegmentId, Box<[u8]>>,
    }

    struct SegmentBytesSchema;

    impl Schema for SegmentBytesSchema {
        const DOMAIN: u8 = 0x52;
        const TYPE: u16 = 0xfffc;
        type Value = [u8];

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    struct FileSegmentStore {
        store: FileStore,
        mapping_path: std::path::PathBuf,
    }

    impl FileSegmentStore {
        fn open(root: &std::path::Path) -> Self {
            let store = FileStore::open(root.join("cas"), 16 * 1024 * 1024)
                .expect("open durable segment CAS");
            Self {
                store,
                mapping_path: root.join("segment-object.map"),
            }
        }
    }

    impl DurableSemanticSegmentStore for FileSegmentStore {
        type Error = String;

        fn commit_and_read(
            &mut self,
            _selection: SelectedSemanticPlane,
            segment: SemanticSegmentId,
            payload: &[u8],
            admit: &mut dyn FnMut(&[u8]) -> Result<(), ReplicationError>,
        ) -> Result<Box<[u8]>, Self::Error> {
            admit(payload).map_err(|error| format!("manifest admission: {error}"))?;
            let key = ObjectKey::<SegmentBytesSchema>::from_value(payload);
            let object = TypedObject::from_value(&key, payload);
            let object_id = self
                .store
                .write_object(&object)
                .map_err(|error| format!("write semantic segment: {error:?}"))?;
            let mut mapping = File::create(&self.mapping_path)
                .map_err(|error| format!("create segment mapping: {error}"))?;
            mapping
                .write_all(segment.as_bytes())
                .and_then(|()| mapping.write_all(object_id.as_bytes()))
                .and_then(|()| mapping.sync_all())
                .map_err(|error| format!("persist segment mapping: {error}"))?;
            let reopened = self
                .store
                .read_object(object_id)
                .map_err(|error| format!("read committed semantic segment: {error:?}"))?;
            Ok(reopened.bytes().to_vec().into_boxed_slice())
        }
    }

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn create() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "backend-ir-hydration-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create hydration test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl DurableSemanticSegmentStore for TestSegmentStore {
        type Error = &'static str;

        fn commit_and_read(
            &mut self,
            _selection: SelectedSemanticPlane,
            segment: SemanticSegmentId,
            payload: &[u8],
            admit: &mut dyn FnMut(&[u8]) -> Result<(), ReplicationError>,
        ) -> Result<Box<[u8]>, Self::Error> {
            admit(payload).map_err(|_| "manifest admission failed")?;
            if self.fail_next {
                self.fail_next = false;
                return Err("injected CAS commit failure");
            }
            self.payloads.insert(segment, Box::from(payload));
            let mut readback = self
                .payloads
                .get(&segment)
                .cloned()
                .ok_or("committed segment disappeared")?;
            if self.corrupt_readback {
                if let Some(first) = readback.first_mut() {
                    *first ^= 0xff;
                }
            }
            Ok(readback)
        }
    }

    fn persist_payload<S: DurableSemanticSegmentStore>(
        cursor: &mut IrHydrationCursor<'_, '_>,
        stamp: SelectedGenerationStamp,
        request: &IrHydrationRequest,
        payload: &[u8],
        store: &mut S,
    ) -> Result<VerifiedSemanticSegment, IrHydrationError> {
        let mut source = TestSelectionSource::new(stamp);
        let pending = cursor.verify_payload(&mut source, request, payload)?;
        cursor.commit_admitted_segment(&mut source, request, pending, store)
    }

    #[test]
    fn sparse_ranges_are_bounded_repeat_after_drop_and_resume_cold() {
        let payload = b"abcdefghij";
        let original = manifest(&[payload], Coverage::Complete, 1);
        let selected = stamp(&original, 1);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let mut cursor =
            new_cursor(&original, selected, kind, &[], limits()).expect("selected cursor");

        let first = first_request(&mut cursor, selected);
        assert_eq!(
            request_range(&first, selected).expect("current range"),
            ByteRange::new(0, 4).expect("range")
        );
        let repeated = first_request(&mut cursor, selected);
        assert_eq!(repeated, first, "a dropped range is safe to retry");
        let checkpoint = cursor_checkpoint(&cursor, selected);
        let token_bytes = checkpoint.encode_cursor().expect("semantic token");
        let token = SemanticHydrationCursorToken::decode(&token_bytes).expect("token reopen");

        // The canonical semantic manifest and cursor token are the only
        // metadata encodings. Sparse byte extents remain in receiving CAS.
        let reopened = SemanticPlaneManifest::decode(&original.encode().expect("manifest encode"))
            .expect("manifest cold reopen");
        assert_eq!(reopened.root(), original.root());
        assert!(matches!(
            IrHydrationCheckpoint::restore(
                &reopened,
                &mut TestSelectionSource::new(stamp(&reopened, 2)),
                selected,
                SemanticPlaneImageKey::from_manifest(0, &reopened),
                kind,
                token,
            ),
            Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale))
        ));
        let checkpoint = restore_checkpoint(&reopened, selected, kind, token)
            .expect("persisted selection and cursor bind");
        assert!(matches!(
            resume_cursor(&reopened, checkpoint, stamp(&reopened, 2), &[], limits(),),
            Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale))
        ));
        let mut resumed = resume_cursor(&reopened, checkpoint, selected, &[], limits())
            .expect("cold cursor resume");

        let mut sparse = empty_coverage();
        sparse
            .insert(
                first
                    .byte_range(&mut TestSelectionSource::new(selected))
                    .expect("current range")
                    .expect("first byte range"),
            )
            .expect("stage");
        let staged = SparseSegmentCoverage::new(
            request_segment(&first, selected).segment_id,
            sparse.clone(),
        );
        let second = match poll(
            &mut resumed,
            selected,
            Some(&staged),
            HydrationCredits::new(1, 3),
        )
        .expect("resumed range")
        {
            IrHydrationPoll::Request(request) => request,
            other => panic!("expected resumed range, got {other:?}"),
        };
        assert_eq!(
            request_range(&second, selected).expect("current range"),
            ByteRange::new(4, 3).expect("range")
        );

        sparse
            .insert(ByteRange::new(4, 6).expect("remaining staged bytes"))
            .expect("stage remainder");
        let complete =
            SparseSegmentCoverage::new(request_segment(&second, selected).segment_id, sparse);
        let local = match poll(
            &mut resumed,
            selected,
            Some(&complete),
            HydrationCredits::new(0, 0),
        )
        .expect("local verify")
        {
            IrHydrationPoll::VerifyLocal(request) => request,
            other => panic!("expected local verification, got {other:?}"),
        };
        let verified = persist_payload(
            &mut resumed,
            selected,
            &local,
            payload,
            &mut TestSegmentStore::default(),
        )
        .expect("durable payload admission");
        assert_eq!(
            expose_payload(&verified, selected).expect("selected bytes"),
            payload
        );
        assert_eq!(cursor_coverage(&resumed, selected).present_segments, 1);
        assert_eq!(
            cursor_terminal(&resumed, selected),
            IrHydrationTerminal::Unverified
        );

        let have = [verified.id()];
        let persisted = cursor_checkpoint(&resumed, selected);
        let restored = restore_checkpoint(
            &reopened,
            selected,
            kind,
            SemanticHydrationCursorToken::decode(
                &persisted.encode_cursor().expect("persisted token"),
            )
            .expect("cursor token reopen"),
        )
        .expect("persisted completed cursor");
        let mut fresh = resume_cursor(&reopened, restored, selected, &have, limits())
            .expect("fresh process resumes verified CAS object");
        assert_eq!(
            cursor_terminal(&fresh, selected),
            IrHydrationTerminal::Unverified
        );
        assert_eq!(
            fresh
                .next_request(
                    &mut TestSelectionSource::new(selected),
                    None,
                    HydrationCredits::new(1, 4),
                )
                .expect("resume past verified prefix"),
            IrHydrationPoll::Exhausted
        );
    }

    #[test]
    fn crash_after_hash_before_cas_commit_does_not_ack_and_retries_after_resume() {
        let payload = b"durable facts";
        let manifest = manifest(&[payload], Coverage::Complete, 1);
        let selected = stamp(&manifest, 3);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let mut cursor =
            new_cursor(&manifest, selected, kind, &[], limits()).expect("selected cursor");
        let request = first_request(&mut cursor, selected);
        let pending = verify_payload(&cursor, selected, &request, payload)
            .expect("hash verifies before storage");
        assert_eq!(
            pending.id().as_bytes(),
            request_segment(&request, selected).segment_id.as_bytes(),
            "the checked digest must match the manifest's untrusted claim bytes"
        );
        assert_eq!(cursor_coverage(&cursor, selected).present_segments, 0);
        assert_eq!(
            cursor_terminal(&cursor, selected),
            IrHydrationTerminal::Pending
        );

        let mut failed_store = TestSegmentStore {
            fail_next: true,
            ..TestSegmentStore::default()
        };
        assert!(matches!(
            commit_pending(&mut cursor, selected, &request, pending, &mut failed_store,),
            Err(IrHydrationError::Storage(_))
        ));
        assert_eq!(cursor_coverage(&cursor, selected).present_segments, 0);
        assert_eq!(
            cursor_terminal(&cursor, selected),
            IrHydrationTerminal::Pending
        );

        // A process crash here persists only the semantic cursor position; no
        // checked-have ID was committed because the CAS did not confirm.
        let token = SemanticHydrationCursorToken::decode(
            &cursor
                .checkpoint(&mut TestSelectionSource::new(selected))
                .expect("current selected checkpoint")
                .encode_cursor()
                .expect("unadvanced cursor token"),
        )
        .expect("reopened cursor token");
        let reopened = SemanticPlaneManifest::decode(&manifest.encode().expect("manifest encode"))
            .expect("manifest cold reopen");
        let checkpoint =
            restore_checkpoint(&reopened, selected, kind, token).expect("checkpoint rebind");
        let mut resumed = resume_cursor(&reopened, checkpoint, selected, &[], limits())
            .expect("fresh cursor after crash");
        let retry = first_request(&mut resumed, selected);
        assert_eq!(retry, request, "uncommitted bytes must be fetched again");
        assert_eq!(
            cursor_terminal(&resumed, selected),
            IrHydrationTerminal::Pending
        );
    }

    #[test]
    fn corrupted_cas_readback_does_not_ack_the_segment() {
        let payload = b"readback must be checked";
        let manifest = manifest(&[payload], Coverage::Complete, 1);
        let selected = stamp(&manifest, 4);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let mut cursor =
            new_cursor(&manifest, selected, kind, &[], limits()).expect("selected cursor");
        let request = first_request(&mut cursor, selected);
        let pending = verify_payload(&cursor, selected, &request, payload)
            .expect("verified transfer payload");
        let mut store = TestSegmentStore {
            corrupt_readback: true,
            ..TestSegmentStore::default()
        };

        assert!(matches!(
            commit_pending(&mut cursor, selected, &request, pending, &mut store),
            Err(IrHydrationError::Semantic(
                SemanticManifestError::SegmentIdentity { .. }
            ))
        ));
        assert_eq!(cursor_coverage(&cursor, selected).present_segments, 0);
        assert_eq!(
            cursor_terminal(&cursor, selected),
            IrHydrationTerminal::Pending
        );
        assert_eq!(first_request(&mut cursor, selected), request);
    }

    #[test]
    fn selection_is_rechecked_after_cas_readback_before_ack() {
        let payload = b"selection can move during storage I/O";
        let manifest = manifest(&[payload], Coverage::Complete, 1);
        let selected = stamp(&manifest, 4);
        let changed = stamp(&manifest, 5);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let mut cursor =
            new_cursor(&manifest, selected, kind, &[], limits()).expect("selected cursor");
        let request = first_request(&mut cursor, selected);
        let mut source = TestSelectionSource::scripted([selected, selected, selected, changed]);
        let pending = cursor
            .verify_payload(&mut source, &request, payload)
            .expect("verify while original generation is selected");
        let mut store = TestSegmentStore::default();

        assert!(matches!(
            cursor.commit_admitted_segment(&mut source, &request, pending, &mut store),
            Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale))
        ));
        assert_eq!(store.payloads.len(), 1, "durable bytes may remain reusable");
        assert_eq!(
            cursor.coverage(&mut TestSelectionSource::new(changed)),
            Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale))
        );
        assert_eq!(
            cursor.terminal(&mut TestSelectionSource::new(changed)),
            Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale))
        );
    }

    #[test]
    fn committed_segment_survives_a_process_restart() {
        let directory = TestDirectory::create();
        for phase in ["commit", "reopen"] {
            let output = Command::new(std::env::current_exe().expect("test executable"))
                .arg("--nocapture")
                .arg("cold_restart_process_child")
                .env("BACKEND_IR_HYDRATION_TEST_ROOT", &directory.0)
                .env("BACKEND_IR_HYDRATION_TEST_PHASE", phase)
                .output()
                .expect("launch cold restart fixture");
            assert!(
                output.status.success(),
                "cold restart child {phase} failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn durable_sparse_range_store_resumes_across_cold_process_restart() {
        let directory = TestDirectory::create();
        for phase in ["stage", "resume"] {
            let output = Command::new(std::env::current_exe().expect("test executable"))
                .arg("--nocapture")
                .arg("file_sparse_cold_restart_process_child")
                .env("BACKEND_IR_SPARSE_TEST_ROOT", &directory.0)
                .env("BACKEND_IR_SPARSE_TEST_PHASE", phase)
                .output()
                .expect("launch sparse cold-restart fixture");
            assert!(
                output.status.success(),
                "sparse restart child {phase} failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn file_sparse_cold_restart_process_child() {
        let Ok(root) = std::env::var("BACKEND_IR_SPARSE_TEST_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let phase = std::env::var("BACKEND_IR_SPARSE_TEST_PHASE").expect("sparse phase");
        let payload = independent_fixture_payload();
        let manifest = manifest(&[&payload], Coverage::Complete, 1);
        let selected = stamp(&manifest, 11);
        let image = SemanticPlaneImageKey::from_manifest(0, &manifest);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let limits = range_store_limits();
        let target = fixture_target(manifest.build().profile());
        match phase.as_str() {
            "stage" => {
                fs::write(
                    root.join("selected.manifest"),
                    manifest
                        .encode()
                        .expect("encode independent fixture manifest"),
                )
                .expect("persist fixture manifest");
                let mut source = TestSelectionSource::with_images(selected, [image]);
                let mut cursor = IrHydrationCursor::new_for_image(
                    &manifest,
                    &mut source,
                    image,
                    kind,
                    &[],
                    limits,
                )
                .expect("select exact catalog image");
                let request = match cursor
                    .next_request(&mut source, None, HydrationCredits::new(1, 16 * 1024))
                    .expect("first bounded request")
                {
                    IrHydrationPoll::Request(request) => request,
                    other => panic!("expected first sparse range, got {other:?}"),
                };
                let get = SemanticRangeGet::from_request(1, target, &request, &mut source)
                    .expect("bind first range to selected image");
                let range = get.byte_range;
                let start = usize::try_from(range.start).expect("small sparse offset");
                let end =
                    usize::try_from(range.end().expect("range end")).expect("small sparse end");
                let chunk = SemanticRangeChunk::for_request(&get, payload[start..end].to_vec());
                let store = FileStore::open(root.join("cas"), 16 * 1024 * 1024)
                    .expect("open production FileStore");
                let mut sparse = FileSemanticRangeStore::open(store, limits)
                    .expect("open sparse FileStore adapter");
                let progress = accept_semantic_range(
                    &mut cursor,
                    &mut source,
                    &request,
                    &get,
                    &chunk,
                    &mut sparse,
                    limits,
                )
                .expect("durably stage first range");
                let SemanticRangeClientProgress::Staged {
                    coverage,
                    checkpoint,
                } = progress
                else {
                    panic!("partial segment must not be exposed as complete");
                };
                assert_eq!(coverage.bytes().covered_bytes(), range.len);
                assert_eq!(cursor_coverage(&cursor, selected).present_segments, 0);
                fs::write(
                    root.join("client.checkpoint"),
                    checkpoint
                        .encode()
                        .expect("encode sparse client checkpoint"),
                )
                .expect("persist client checkpoint before process exit");
            }
            "resume" => {
                let manifest = SemanticPlaneManifest::decode(
                    &fs::read(root.join("selected.manifest")).expect("read selected manifest"),
                )
                .expect("cold-open selected manifest");
                let selected = stamp(&manifest, 11);
                let image = SemanticPlaneImageKey::from_manifest(0, &manifest);
                let mut source = TestSelectionSource::with_images(selected, [image]);
                let checkpoint = SemanticRangeClientCheckpoint::decode(
                    &fs::read(root.join("client.checkpoint")).expect("read client checkpoint"),
                )
                .expect("decode cold client checkpoint");
                let store = FileStore::open(root.join("cas"), 16 * 1024 * 1024)
                    .expect("cold-open FileStore");
                let mut sparse = FileSemanticRangeStore::open(store, limits)
                    .expect("cold-open sparse FileStore adapter");
                let (mut cursor, partial, poll) = checkpoint
                    .resume(&manifest, &mut source, &[], limits, &mut sparse)
                    .expect("resume cursor and durable sparse extents");
                assert_eq!(partial.bytes().covered_bytes(), 16 * 1024);
                let request = match poll {
                    IrHydrationPoll::Request(request) => request,
                    other => panic!("expected resumed missing range, got {other:?}"),
                };
                let get = SemanticRangeGet::from_request(2, target, &request, &mut source)
                    .expect("bind resumed range to same image");
                let range = get.byte_range;
                assert_eq!(range.start, 16 * 1024);
                let start = usize::try_from(range.start).expect("small sparse offset");
                let end =
                    usize::try_from(range.end().expect("range end")).expect("small sparse end");
                let chunk = SemanticRangeChunk::for_request(&get, payload[start..end].to_vec());
                let progress = accept_semantic_range(
                    &mut cursor,
                    &mut source,
                    &request,
                    &get,
                    &chunk,
                    &mut sparse,
                    limits,
                )
                .expect("admit and durably commit full segment");
                let SemanticRangeClientProgress::Complete(segment) = progress else {
                    panic!("all bytes should complete one segment");
                };
                assert_eq!(
                    segment
                        .payload(&mut source)
                        .expect("still-selected payload"),
                    payload,
                    "independent fixture bytes must survive sparse restart exactly"
                );
                assert_eq!(cursor_coverage(&cursor, selected).present_segments, 1);
            }
            other => panic!("unknown sparse restart phase {other}"),
        }
    }

    #[test]
    fn cold_restart_process_child() {
        let Ok(root) = std::env::var("BACKEND_IR_HYDRATION_TEST_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let phase = std::env::var("BACKEND_IR_HYDRATION_TEST_PHASE").expect("cold restart phase");
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let payload = b"cross-process semantic plane segment";

        match phase.as_str() {
            "commit" => {
                let manifest = manifest(&[payload], Coverage::Complete, 1);
                fs::write(
                    root.join("manifest.bin"),
                    manifest.encode().expect("encode selected manifest"),
                )
                .expect("persist semantic manifest");
                let selected = stamp(&manifest, 5);
                let mut cursor = new_cursor(&manifest, selected, kind, &[], limits())
                    .expect("start process fixture cursor");
                let request = first_request(&mut cursor, selected);
                let mut store = FileSegmentStore::open(&root);
                let admitted =
                    persist_payload(&mut cursor, selected, &request, payload, &mut store)
                        .expect("durable CAS commit and read-back");
                assert_eq!(
                    expose_payload(&admitted, selected).expect("current payload"),
                    payload
                );
                fs::write(
                    root.join("cursor.bin"),
                    cursor
                        .checkpoint(&mut TestSelectionSource::new(selected))
                        .expect("current selected checkpoint")
                        .encode_cursor()
                        .expect("encode durable cursor"),
                )
                .expect("persist semantic cursor");
                assert_eq!(cursor_coverage(&cursor, selected).present_segments, 1);
            }
            "reopen" => {
                let manifest = SemanticPlaneManifest::decode(
                    &fs::read(root.join("manifest.bin")).expect("read manifest after restart"),
                )
                .expect("reopen canonical semantic manifest");
                let selected = stamp(&manifest, 5);
                let cursor_token = SemanticHydrationCursorToken::decode(
                    &fs::read(root.join("cursor.bin")).expect("read cursor after restart"),
                )
                .expect("reopen canonical semantic cursor");
                let mapping =
                    fs::read(root.join("segment-object.map")).expect("read durable CAS mapping");
                assert_eq!(mapping.len(), 64, "mapping binds segment and CAS object");
                let mut segment_bytes = [0; 32];
                segment_bytes.copy_from_slice(&mapping[..32]);
                let mut object_bytes = [0; 32];
                object_bytes.copy_from_slice(&mapping[32..]);
                let store = FileStore::open(root.join("cas"), 16 * 1024 * 1024)
                    .expect("reopen durable CAS");
                let object = store
                    .read_object_claim(UntrustedObjectId::from_bytes(object_bytes))
                    .expect("admit persisted segment CAS object");
                let segment = manifest
                    .plane(kind)
                    .expect("selected IR plane")
                    .segments()
                    .first()
                    .expect("selected segment");
                let admitted_id = segment
                    .admit(kind, object.bytes())
                    .expect("re-admit segment bytes after process restart");
                assert_eq!(admitted_id.as_bytes(), &segment_bytes);

                let have = [admitted_id];
                let checkpoint = restore_checkpoint(&manifest, selected, kind, cursor_token)
                    .expect("restore exact selected cursor");
                let mut cursor = resume_cursor(&manifest, checkpoint, selected, &have, limits())
                    .expect("resume with CAS-verified segment");
                assert_eq!(cursor_coverage(&cursor, selected).present_segments, 1);
                assert_eq!(
                    cursor_terminal(&cursor, selected),
                    IrHydrationTerminal::Unverified
                );
                assert_eq!(
                    cursor
                        .next_request(
                            &mut TestSelectionSource::new(selected),
                            None,
                            HydrationCredits::new(1, 4),
                        )
                        .expect("cursor past durable segment"),
                    IrHydrationPoll::Exhausted
                );
            }
            other => panic!("unknown cold restart phase {other}"),
        }
    }

    #[test]
    fn file_sparse_ranges_fail_closed_on_stale_selection_and_corrupt_full_segment() {
        let payload = independent_fixture_payload();
        let manifest = manifest(&[&payload], Coverage::Complete, 1);
        let selected = stamp(&manifest, 15);
        let moved = stamp(&manifest, 16);
        let image = SemanticPlaneImageKey::from_manifest(0, &manifest);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let limits = range_store_limits();
        let target = fixture_target(manifest.build().profile());

        let stale_directory = TestDirectory::create();
        let mut source = TestSelectionSource::with_images(selected, [image]);
        let mut cursor =
            IrHydrationCursor::new_for_image(&manifest, &mut source, image, kind, &[], limits)
                .expect("select fixture image");
        let request = match cursor
            .next_request(&mut source, None, HydrationCredits::new(1, 16 * 1024))
            .expect("first range")
        {
            IrHydrationPoll::Request(request) => request,
            other => panic!("expected range, got {other:?}"),
        };
        let get = SemanticRangeGet::from_request(51, target.clone(), &request, &mut source)
            .expect("first live get");
        let range = get.byte_range;
        let start = usize::try_from(range.start).expect("small offset");
        let end = usize::try_from(range.end().expect("range end")).expect("small end");
        let chunk = SemanticRangeChunk::for_request(&get, payload[start..end].to_vec());
        let cas = FileStore::open(stale_directory.0.join("cas"), 16 * 1024 * 1024)
            .expect("open stale fixture CAS");
        let mut sparse = FileSemanticRangeStore::open(cas, limits).expect("open sparse store");
        let mut moved_source = TestSelectionSource::scripted([selected, selected, selected, moved]);
        // `get` construction completed before the scripted current reads.
        moved_source.members = Some(vec![image]);
        assert!(matches!(
            accept_semantic_range(
                &mut cursor,
                &mut moved_source,
                &request,
                &get,
                &chunk,
                &mut sparse,
                limits,
            ),
            Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale))
        ));
        assert_eq!(cursor_coverage(&cursor, selected).present_segments, 0);

        let corrupt_directory = TestDirectory::create();
        let mut source = TestSelectionSource::with_images(selected, [image]);
        let mut cursor =
            IrHydrationCursor::new_for_image(&manifest, &mut source, image, kind, &[], limits)
                .expect("select fixture image for corrupt range");
        let request = match cursor
            .next_request(&mut source, None, HydrationCredits::new(1, 16 * 1024))
            .expect("first range")
        {
            IrHydrationPoll::Request(request) => request,
            other => panic!("expected range, got {other:?}"),
        };
        let get = SemanticRangeGet::from_request(52, target.clone(), &request, &mut source)
            .expect("first get");
        let range = get.byte_range;
        let start = usize::try_from(range.start).expect("small offset");
        let end = usize::try_from(range.end().expect("range end")).expect("small end");
        let chunk = SemanticRangeChunk::for_request(&get, payload[start..end].to_vec());
        let cas = FileStore::open(corrupt_directory.0.join("cas"), 16 * 1024 * 1024)
            .expect("open corrupt fixture CAS");
        let mut sparse = FileSemanticRangeStore::open(cas, limits).expect("open sparse store");
        let progress = accept_semantic_range(
            &mut cursor,
            &mut source,
            &request,
            &get,
            &chunk,
            &mut sparse,
            limits,
        )
        .expect("first range is durable");
        let SemanticRangeClientProgress::Staged { checkpoint, .. } = progress else {
            panic!("partial segment must remain unacknowledged");
        };
        let (mut cursor, _, request_poll) = checkpoint
            .resume(&manifest, &mut source, &[], limits, &mut sparse)
            .expect("resume local sparse transfer");
        let request = match request_poll {
            IrHydrationPoll::Request(request) => request,
            other => panic!("expected missing tail range, got {other:?}"),
        };
        let get =
            SemanticRangeGet::from_request(53, target, &request, &mut source).expect("tail get");
        let range = get.byte_range;
        let start = usize::try_from(range.start).expect("small offset");
        let end = usize::try_from(range.end().expect("range end")).expect("small end");
        let mut bad_bytes = payload[start..end].to_vec();
        bad_bytes[0] ^= 0xff;
        let bad_chunk = SemanticRangeChunk::for_request(&get, bad_bytes);
        assert!(matches!(
            accept_semantic_range(
                &mut cursor,
                &mut source,
                &request,
                &get,
                &bad_chunk,
                &mut sparse,
                limits,
            ),
            Err(IrHydrationError::Semantic(
                SemanticManifestError::SegmentIdentity { .. }
            ))
        ));
        assert_eq!(cursor_coverage(&cursor, selected).present_segments, 0);
        assert_eq!(
            cursor_terminal(&cursor, selected),
            IrHydrationTerminal::Pending
        );
        let retried = match cursor
            .next_request(&mut source, None, HydrationCredits::new(1, 16 * 1024))
            .expect("discarded corruption is requested again")
        {
            IrHydrationPoll::Request(request) => request,
            other => panic!("expected clean retry, got {other:?}"),
        };
        assert_eq!(
            request_range(&retried, selected),
            Some(ByteRange::new(0, 16 * 1024).expect("first range"))
        );
    }

    #[test]
    fn production_sparse_store_rehydrates_after_gc_reclaims_a_mapped_segment() {
        let payload = b"semantic segment rehydrated after CAS collection";
        let manifest = manifest(&[payload], Coverage::Complete, 1);
        let selected = stamp(&manifest, 39);
        let image = SemanticPlaneImageKey::from_manifest(0, &manifest);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let limits = range_store_limits();
        let target = fixture_target(manifest.build().profile());
        let directory = TestDirectory::create();
        let cas_root = directory.0.join("cas");
        let cas =
            FileStore::open(&cas_root, 16 * 1024 * 1024).expect("open production semantic CAS");
        let mut source = TestSelectionSource::with_images(selected, [image]);
        let mut cursor =
            IrHydrationCursor::new_for_image(&manifest, &mut source, image, kind, &[], limits)
                .expect("select semantic plane");
        let request = match cursor
            .next_request(
                &mut source,
                None,
                HydrationCredits::new(1, payload.len() as u64),
            )
            .expect("request complete segment")
        {
            IrHydrationPoll::Request(request) => request,
            other => panic!("expected full-segment request, got {other:?}"),
        };
        let range_request = request
            .segment(&mut source)
            .expect("exact selected segment request");
        let selection = request.selection();
        let get = SemanticRangeGet::from_request(71, target.clone(), &request, &mut source)
            .expect("bind selected full-segment request");
        assert_eq!(
            get.byte_range,
            ByteRange::new(0, payload.len() as u64).expect("bounded full segment range")
        );
        let chunk = SemanticRangeChunk::for_request(&get, payload.to_vec());
        let mut sparse = FileSemanticRangeStore::open(cas.clone(), limits)
            .expect("open production sparse adapter");
        let first = accept_semantic_range(
            &mut cursor,
            &mut source,
            &request,
            &get,
            &chunk,
            &mut sparse,
            limits,
        )
        .expect("commit first semantic segment through production adapter");
        assert!(matches!(first, SemanticRangeClientProgress::Complete(_)));
        assert!(
            crate::DurableSemanticRangeStore::read_complete_segment(
                &mut sparse,
                selection,
                range_request,
            )
            .expect("read mapped segment before collection")
            .is_some()
        );
        drop(sparse);

        let report = cas
            .collect_garbage(&GcRoots::new(), GcLimits::default())
            .expect("collect unrooted semantic object");
        assert!(
            report.swept_items > 0,
            "the segment object must be reclaimed"
        );
        drop(cas);

        // A cold adapter sees the durable mapping, proves its CAS member is
        // absent, removes only that unchanged stale record, and requests the
        // exact bytes from the selected source again.
        let cold_cas = FileStore::open(&cas_root, 16 * 1024 * 1024)
            .expect("cold-reopen production semantic CAS");
        let mut cold =
            FileSemanticRangeStore::open(cold_cas, limits).expect("cold-reopen sparse adapter");
        assert!(
            crate::DurableSemanticRangeStore::read_complete_segment(
                &mut cold,
                selection,
                range_request,
            )
            .expect("missing GC member becomes a local cache miss")
            .is_none()
        );
        let mapping_dir = cas_root.join("semantic-hydration/mappings");
        assert_eq!(
            fs::read_dir(mapping_dir)
                .expect("read map directory")
                .count(),
            0,
            "stale map must be durably invalidated"
        );

        let mut cold_source = TestSelectionSource::with_images(selected, [image]);
        let mut cold_cursor =
            IrHydrationCursor::new_for_image(&manifest, &mut cold_source, image, kind, &[], limits)
                .expect("restart selected hydration cursor");
        let cold_request = match cold_cursor
            .next_request(
                &mut cold_source,
                None,
                HydrationCredits::new(1, payload.len() as u64),
            )
            .expect("request rehydrated segment")
        {
            IrHydrationPoll::Request(request) => request,
            other => panic!("expected full-segment retry, got {other:?}"),
        };
        let cold_get = SemanticRangeGet::from_request(72, target, &cold_request, &mut cold_source)
            .expect("bind cold exact request");
        let cold_chunk = SemanticRangeChunk::for_request(&cold_get, payload.to_vec());
        let progress = accept_semantic_range(
            &mut cold_cursor,
            &mut cold_source,
            &cold_request,
            &cold_get,
            &cold_chunk,
            &mut cold,
            limits,
        )
        .expect("rehydrate and admit through production sparse adapter");
        let SemanticRangeClientProgress::Complete(segment) = progress else {
            panic!("rehydrated complete segment must be admitted");
        };
        assert_eq!(
            segment.payload(&mut cold_source).expect("current payload"),
            payload
        );
        assert_eq!(cursor_coverage(&cold_cursor, selected).present_segments, 1);
    }

    #[test]
    fn catalog_membership_rejects_cross_mixed_image_identity() {
        let manifest_a = manifest(&[b"image A payload"], Coverage::Complete, 1);
        let manifest_b = manifest(&[b"image B payload"], Coverage::Complete, 1);
        let image_a = SemanticPlaneImageKey::from_manifest(4, &manifest_a);
        let image_b = SemanticPlaneImageKey::from_manifest(9, &manifest_b);
        let selected = catalog_stamp(&[(4, &manifest_a), (9, &manifest_b)], 21);
        let mut source = TestSelectionSource::with_images(selected, [image_a, image_b]);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);

        let mut cursor = IrHydrationCursor::new_for_image(
            &manifest_a,
            &mut source,
            image_a,
            kind,
            &[],
            limits(),
        )
        .expect("selected A image");
        let crossed = SemanticPlaneImageKey::new(
            image_a.artifact_ordinal(),
            image_b.semantic_generation(),
            image_b.manifest_root(),
        );
        assert!(matches!(
            IrHydrationCursor::new_for_image(
                &manifest_b,
                &mut source,
                crossed,
                kind,
                &[],
                limits(),
            ),
            Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale))
        ));

        let request = first_request(&mut cursor, selected);
        let mut range_get = SemanticRangeGet::from_request(
            17,
            fixture_target(manifest_a.build().profile()),
            &request,
            &mut source,
        )
        .expect("A range get");
        range_get.image = image_b;
        assert_eq!(
            range_get.encode(),
            Err(ReplicationError::IdentityMismatch),
            "an aggregate catalog root cannot substitute for a different image root"
        );
    }

    #[test]
    fn changed_selection_and_corruption_do_not_expose_or_acknowledge_bytes() {
        let payload = b"abcdefghij";
        let value = manifest(&[payload], Coverage::Complete, 1);
        let selected = stamp(&value, 7);
        let changed_head = stamp(&value, 8);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let mut cursor =
            new_cursor(&value, selected, kind, &[], limits()).expect("selected cursor");
        let request = first_request(&mut cursor, selected);

        assert_eq!(
            cursor.next_request(
                &mut TestSelectionSource::new(changed_head),
                None,
                HydrationCredits::new(1, 4),
            ),
            Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale))
        );
        for corrupted in [b"abcdabcdij".as_slice(), b"ghijabcdef".as_slice()] {
            assert!(matches!(
                verify_payload(&cursor, selected, &request, corrupted),
                Err(IrHydrationError::Semantic(
                    SemanticManifestError::SegmentIdentity { .. }
                ))
            ));
            assert_eq!(cursor_coverage(&cursor, selected).missing_segments, 1);
        }

        let local = IrHydrationRequest {
            byte_range: None,
            ..request
        };
        let verified = persist_payload(
            &mut cursor,
            selected,
            &local,
            payload,
            &mut TestSegmentStore::default(),
        )
        .expect("good bytes durably admit");
        assert_eq!(
            expose_payload(&verified, changed_head),
            Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale))
        );
        assert_eq!(expose_payload(&verified, selected), Ok(payload.as_slice()));
    }

    #[test]
    fn later_ranges_and_duplicate_acknowledgements_are_rejected() {
        let payloads: [&[u8]; 2] = [b"first", b"second"];
        let value = manifest(&payloads, Coverage::Complete, 1);
        let selected = stamp(&value, 1);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let mut receiving =
            new_cursor(&value, selected, kind, &[], limits()).expect("receiving cursor");
        let first = first_request(&mut receiving, selected);

        let mut other = new_cursor(&value, selected, kind, &[], limits()).expect("second cursor");
        let other_first = first_request(&mut other, selected);
        persist_payload(
            &mut other,
            selected,
            &other_first,
            payloads[0],
            &mut TestSegmentStore::default(),
        )
        .expect("first segment");
        let second = first_request(&mut other, selected);

        let pending_second = verify_payload(&receiving, selected, &second, payloads[1]);
        assert!(matches!(
            pending_second,
            Err(IrHydrationError::OutOfOrderSegment)
        ));
        persist_payload(
            &mut receiving,
            selected,
            &first,
            payloads[0],
            &mut TestSegmentStore::default(),
        )
        .expect("first segment");
        assert!(matches!(
            verify_payload(&receiving, selected, &first, payloads[0]),
            Err(IrHydrationError::NoPendingSegment)
        ));
        let next = first_request(&mut receiving, selected);
        let expected = value
            .plane(kind)
            .expect("plane")
            .segments()
            .get(1)
            .expect("second segment")
            .id_claim();
        assert_eq!(request_segment(&next, selected).segment_id, expected);
    }

    #[test]
    fn out_of_order_sparse_extents_request_only_the_oldest_missing_interval() {
        let payload = b"abcdefghij";
        let value = manifest(&[payload], Coverage::Complete, 1);
        let selected = stamp(&value, 1);
        let kind = SemanticPlaneKind::Ir(SemanticIrPlane::Core);
        let mut cursor =
            new_cursor(&value, selected, kind, &[], limits()).expect("selected cursor");
        let logical = first_request(&mut cursor, selected)
            .segment(&mut TestSelectionSource::new(selected))
            .expect("current segment");

        let out_of_order =
            SparseCoverage::from_ranges([ByteRange::new(4, 2).expect("later extent")], 8)
                .expect("sparse extent");
        let later_extent = SparseSegmentCoverage::new(logical.segment_id, out_of_order.clone());
        let first_missing = match poll(
            &mut cursor,
            selected,
            Some(&later_extent),
            HydrationCredits::new(1, 4),
        )
        .expect("oldest missing interval")
        {
            IrHydrationPoll::Request(request) => request,
            other => panic!("expected range request, got {other:?}"),
        };
        assert_eq!(
            request_range(&first_missing, selected).expect("current range"),
            ByteRange::new(0, 4).expect("first missing range")
        );

        let mut coalesced = out_of_order;
        coalesced
            .insert(ByteRange::new(0, 4).expect("earlier extent"))
            .expect("coalesce sparse extents");
        let staged_coverage = SparseSegmentCoverage::new(logical.segment_id, coalesced);
        let next_missing = match poll(
            &mut cursor,
            selected,
            Some(&staged_coverage),
            HydrationCredits::new(1, 4),
        )
        .expect("next missing interval")
        {
            IrHydrationPoll::Request(request) => request,
            other => panic!("expected next range request, got {other:?}"),
        };
        assert_eq!(
            request_range(&next_missing, selected).expect("current range"),
            ByteRange::new(6, 4).expect("remaining range")
        );
    }

    #[test]
    fn absent_and_unavailable_planes_are_terminal_and_reuse_requires_exact_build() {
        let value = manifest(&[b"facts"], Coverage::Complete, 1);
        let selected = stamp(&value, 1);
        assert_eq!(
            new_cursor(
                &value,
                selected,
                SemanticPlaneKind::Ir(SemanticIrPlane::Types),
                &[],
                limits(),
            )
            .err(),
            Some(IrHydrationError::Terminal(IrHydrationTerminal::Missing))
        );

        let unavailable = manifest(&[b"facts"], Coverage::Unavailable, 1);
        let unavailable_stamp = stamp(&unavailable, 1);
        assert_eq!(
            new_cursor(
                &unavailable,
                unavailable_stamp,
                SemanticPlaneKind::Ir(SemanticIrPlane::Core),
                &[],
                limits(),
            )
            .err(),
            Some(IrHydrationError::Terminal(IrHydrationTerminal::Unavailable))
        );

        let changed_toolchain = manifest(&[b"facts"], Coverage::Complete, 2);
        assert_eq!(
            local_build_reuse_eligible(&value, &value, value.root()),
            Ok(false),
            "claim-only read evidence cannot authorize compiler reuse"
        );
        assert_eq!(
            local_build_reuse_eligible(&value, &changed_toolchain, value.root()),
            Ok(false),
            "a changed target platform invalidates reuse"
        );
        assert_eq!(
            local_build_reuse_eligible(&value, &value, changed_toolchain.root()),
            Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale))
        );
    }

    #[test]
    fn local_compile_reuse_requires_matching_admitted_read_and_build_witnesses() {
        let base = admitted_manifest(b"facts", 10, 1);
        let exact_target = admitted_manifest(b"facts", 10, 1);
        assert!(base.claims_admitted());
        assert!(exact_target.claims_admitted());
        assert_eq!(
            local_build_reuse_eligible(&base, &exact_target, base.root()),
            Ok(true)
        );

        let changed_input = admitted_manifest(b"facts", 11, 1);
        assert_eq!(
            local_build_reuse_eligible(&base, &changed_input, base.root()),
            Ok(false),
            "same bytes with a changed read frontier are not a cache hit"
        );
        let changed_platform = admitted_manifest(b"facts", 10, 2);
        assert_eq!(
            local_build_reuse_eligible(&base, &changed_platform, base.root()),
            Ok(false),
            "a different target platform is not the same compiler build"
        );

        let base_stamp = stamp(&base, 1);
        let target_stamp = stamp(&exact_target, 2);
        let mut base_source = TestSelectionSource::new(base_stamp);
        let mut target_source = TestSelectionSource::new(target_stamp);
        let mut delta = SelectedSemanticDeltaCursor::new(
            &base,
            &mut base_source,
            &exact_target,
            &mut target_source,
            base.root(),
        )
        .expect("selected delta");
        let reuse = delta
            .next_action(&mut base_source, &mut target_source)
            .expect("current heads")
            .expect("same segment action");
        assert!(matches!(
            reuse.action(&mut base_source, &mut target_source),
            Ok(SemanticDeltaAction::Reuse { .. })
        ));
        assert_eq!(
            delta.next_action(
                &mut base_source,
                &mut TestSelectionSource::new(stamp(&exact_target, 3)),
            ),
            Err(IrHydrationError::Terminal(IrHydrationTerminal::Stale))
        );
    }
}
