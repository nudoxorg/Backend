//! Adaptive residency for exact semantic-plane segments.
//!
//! Segment bytes are keyed by their admitted content identity. A selected
//! authority binding is carried separately and checked again whenever bytes
//! are exposed. The hot path lends a borrow through `&mut` policy ownership;
//! an `Arc` is created only for a lease that outlives that borrow.

use std::collections::HashMap;
use std::fmt;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use backend_semantic::ir::{
    SemanticDeltaAction, SemanticDeltaCursor, SemanticManifestRoot, SemanticPlaneKind,
    SemanticPlaneManifest, SemanticPlaneSegment, SemanticRangeRequest, SemanticSegmentId,
    UntrustedSemanticSegmentId,
};

use crate::{DurableSemanticRangeStore, SelectedGenerationSource, SelectedSemanticPlane};

/// A cold route periodically samples a valid delta even after pristine CAS
/// wins, so changing storage locality cannot permanently freeze the choice.
const DELTA_REPROBE_INTERVAL: u32 = 32;

/// Bounds for retained semantic segments and delta planning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrResidencyLimits {
    /// Maximum bytes owned by hot entries and their outstanding leases.
    pub hot_bytes: u64,
    /// Maximum distinct hot-owner bytes that may have outstanding leases.
    pub pinned_bytes: u64,
    /// Maximum hot entries and reuse observations retained by this policy.
    pub entries: usize,
    /// Number of uses observed before a segment can become hot.
    pub warm_uses: u32,
    /// Minimum measured cold-read time that makes hot retention useful.
    pub warm_cold_cost: Duration,
    /// Maximum manifest transitions considered for one delta route.
    pub delta_hops: usize,
    /// Maximum aggregate fetched and removed bytes accepted for a delta route.
    pub delta_changed_bytes: u64,
    /// Maximum semantic actions scanned while planning one route.
    pub delta_actions: usize,
}

impl Default for IrResidencyLimits {
    fn default() -> Self {
        Self {
            hot_bytes: 64 * 1024 * 1024,
            pinned_bytes: 16 * 1024 * 1024,
            entries: 256,
            warm_uses: 2,
            warm_cold_cost: Duration::from_micros(40),
            delta_hops: 4,
            delta_changed_bytes: 8 * 1024 * 1024,
            delta_actions: 20_000,
        }
    }
}

/// One historical transition in a checked semantic-plane delta route.
///
/// `base_selection` names the exact historical owner binding used to address
/// its local CAS entry. It is not treated as the current authority for the
/// target; the final target selection is freshly checked before exposure.
#[derive(Clone, Copy, Debug)]
pub struct IrResidencyDeltaHop<'manifest> {
    base: &'manifest SemanticPlaneManifest,
    target: &'manifest SemanticPlaneManifest,
    base_selection: SelectedSemanticPlane,
    expected_base_root: SemanticManifestRoot,
}

impl<'manifest> IrResidencyDeltaHop<'manifest> {
    /// Binds one transition to the expected content root of its base.
    #[must_use]
    pub const fn new(
        base: &'manifest SemanticPlaneManifest,
        target: &'manifest SemanticPlaneManifest,
        base_selection: SelectedSemanticPlane,
        expected_base_root: SemanticManifestRoot,
    ) -> Self {
        Self {
            base,
            target,
            base_selection,
            expected_base_root,
        }
    }
}

/// Checked cost summary for a bounded segment-aware delta route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IrResidencyDeltaSummary {
    /// Number of contiguous manifest transitions checked.
    pub hops: usize,
    /// Number of actions scanned across all planes in this route.
    pub actions: usize,
    /// Number of exact segment identities reusable by the cursor.
    pub reused_segments: usize,
    /// Bytes fetched or removed across the route.
    pub changed_bytes: u64,
    /// Whether the last transition authorizes reusing this exact target segment.
    pub target_segment_reused: bool,
}

/// A validated, borrowed delta route that can be reused for many exact
/// segment reads from the same target manifest.
///
/// Preparation checks every hop and scans its bounded action stream once.
/// The plan retains only the final borrowed base binding and indices of the
/// descriptors the cursor proved reusable in the selected plane; it never
/// retains or copies segment payload bytes. Its index list is bounded by
/// `IrResidencyLimits::delta_actions`.
#[derive(Debug)]
pub struct PreparedIrResidencyDeltaRoute<'manifest> {
    kind: SemanticPlaneKind,
    target_root: SemanticManifestRoot,
    final_hop: Option<IrResidencyDeltaHop<'manifest>>,
    evaluation: DeltaEvaluation,
    target_segments: &'manifest [SemanticPlaneSegment],
    reused_target_segment_indices: Vec<usize>,
}

impl PreparedIrResidencyDeltaRoute<'_> {
    fn evaluate(
        &self,
        selection: SelectedSemanticPlane,
        target: &SemanticPlaneManifest,
        requested: &SemanticPlaneSegment,
    ) -> DeltaEvaluation {
        if self.kind != selection.kind() || self.target_root != target.root() {
            return DeltaEvaluation::Rejected(DeltaReject::Claim);
        }
        match self.evaluation {
            DeltaEvaluation::Eligible(mut summary) => {
                let target_position = self
                    .target_segments
                    .partition_point(|candidate| candidate.first_key() < requested.first_key());
                let is_exact_target = self.target_segments.get(target_position) == Some(requested);
                summary.target_segment_reused = is_exact_target
                    && self
                        .reused_target_segment_indices
                        .binary_search(&target_position)
                        .is_ok();
                DeltaEvaluation::Eligible(summary)
            }
            rejected => rejected,
        }
    }
}

/// Why an otherwise safe lookup uses the selected root's pristine CAS path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrResidencyCasReason {
    /// No delta route was supplied.
    NoDeltaRoute,
    /// The route exceeded a configured work or compaction bound.
    DeltaBoundExceeded,
    /// A root, coverage, plane, or cursor claim did not support reuse.
    DeltaRejected,
    /// The route was valid but did not reuse the requested exact segment.
    SegmentChanged,
    /// A candidate delta CAS object was absent or corrupt.
    DeltaCandidateUnavailable,
    /// Measured route cost favored the exact target CAS path.
    DeltaCostHigher,
    /// The policy read the exact target once to establish a cold baseline.
    DeltaBaselineProbe,
    /// Route fanout exceeded the adaptive policy's lower cost threshold.
    DeltaFanoutHigh,
}

/// Chosen segment source. Delta indicates exact segment reuse from a checked
/// base CAS; it does not describe or construct a composite semantic reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IrResidencyPath {
    /// Borrowed bytes came from the in-memory owner cache.
    HotMemory,
    /// Exact unchanged bytes came from a base CAS proven by the delta cursor.
    DeltaCas(IrResidencyDeltaSummary),
    /// Bytes came from the target selection's exact CAS entry.
    PristineCas(IrResidencyCasReason),
}

/// Current policy counters. `live_owner_bytes` includes owners held by leases
/// after cache eviction, so it does not imply the process RSS is smaller than
/// the cache budget.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IrResidencyMetrics {
    /// Payload bytes retained directly by the hot cache.
    pub hot_bytes: u64,
    /// Live payload allocations, including externally pinned leases.
    pub live_owner_bytes: u64,
    /// Distinct owner bytes with one or more outstanding leases.
    pub pinned_owner_bytes: u64,
    /// Reads served directly from a hot owner.
    pub hot_hits: u64,
    /// Complete segment reads attempted from the range store.
    pub cold_reads: u64,
    /// Exact unchanged segment reads served from a base CAS entry.
    pub delta_reads: u64,
    /// Cumulative measured cursor-planning time for admitted delta routes.
    pub delta_plan_ns: u64,
    /// Cumulative semantic actions scanned for admitted delta routes.
    pub delta_actions_scanned: u64,
    /// Cumulative fetched and removed bytes examined for admitted routes.
    pub delta_changed_bytes_scanned: u64,
    /// CAS objects that failed the requested segment commitment.
    pub corrupt_cas_objects: u64,
    /// Historical CAS reads that failed before payload verification; the
    /// selected target CAS is tried instead.
    pub unreadable_delta_candidates: u64,
    /// Hot owners rejected because retained identity metadata disagreed.
    pub corrupt_hot_owners: u64,
    /// Owners moved from transient reads into bounded hot memory.
    pub hot_admissions: u64,
    /// Hot admissions refused by size, budget, or pin pressure.
    pub admission_rejections: u64,
    /// Hot entries removed to satisfy byte or entry limits.
    pub evictions: u64,
    /// Delta routes rejected during validation or candidate lookup.
    pub delta_rejections: u64,
    /// Leases issued for owners that outlive a cache borrow.
    pub leases_issued: u64,
    /// Reads returned to a caller's borrowed callback.
    pub borrowed_reads: u64,
}

/// Error returned before bytes can be exposed from the residency layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IrResidencyError {
    /// The fresh selected frontier no longer names this exact owner binding.
    StaleSelection,
    /// The selected authority could not be queried.
    Frontier(String),
    /// The selected manifest or plane differs from the admitted selection.
    SelectionManifestMismatch,
    /// The requested segment descriptor is not in the exact selected plane.
    SegmentNotInManifest,
    /// No complete bytes exist in the requested local CAS entry.
    MissingSegment,
    /// Issuing another distinct lease would exceed the pinned-byte limit.
    PinnedBudgetExceeded,
    /// A CAS payload failed its semantic content commitment.
    CorruptCas,
    /// A retained in-memory owner failed its original segment commitment.
    CorruptHotOwner,
    /// A storage adapter failed before returning bytes.
    Storage(String),
}

impl fmt::Display for IrResidencyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "IR residency error: {self:?}")
    }
}

impl std::error::Error for IrResidencyError {}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SegmentLookupKey {
    plane: SemanticPlaneKind,
    claimed_id: UntrustedSemanticSegmentId,
}

impl SegmentLookupKey {
    const fn from_descriptor(plane: SemanticPlaneKind, segment: &SemanticPlaneSegment) -> Self {
        Self {
            plane,
            claimed_id: segment.id_claim(),
        }
    }
}

struct SegmentOwner {
    id: SemanticSegmentId,
    bytes: Box<[u8]>,
    ledger: Option<Weak<ResidencyLedger>>,
}

#[derive(Default)]
struct ResidencyLedger {
    live_owner_bytes: AtomicU64,
    pinned_owner_bytes: AtomicU64,
}

struct PinnedOwnerGuard {
    bytes: u64,
    ledger: Arc<ResidencyLedger>,
}

impl Drop for PinnedOwnerGuard {
    fn drop(&mut self) {
        self.ledger
            .pinned_owner_bytes
            .fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

impl SegmentOwner {
    fn new(
        id: SemanticSegmentId,
        bytes: Box<[u8]>,
        ledger: Option<&Arc<ResidencyLedger>>,
    ) -> Box<Self> {
        if let Some(ledger) = ledger {
            ledger
                .live_owner_bytes
                .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }
        Box::new(Self {
            id,
            bytes,
            ledger: ledger.map(Arc::downgrade),
        })
    }
}

impl Drop for SegmentOwner {
    fn drop(&mut self) {
        if let Some(ledger) = self.ledger.as_ref().and_then(Weak::upgrade) {
            ledger
                .live_owner_bytes
                .fetch_sub(self.bytes.len() as u64, Ordering::Relaxed);
        }
    }
}

enum HotOwner {
    Owned(Box<SegmentOwner>),
    Shared(Arc<SegmentOwner>),
    Vacant,
}

impl HotOwner {
    fn id(&self) -> Option<SemanticSegmentId> {
        match self {
            Self::Owned(owner) => Some(owner.id),
            Self::Shared(owner) => Some(owner.id),
            Self::Vacant => None,
        }
    }

    fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Owned(owner) => Some(&owner.bytes),
            Self::Shared(owner) => Some(&owner.bytes),
            Self::Vacant => None,
        }
    }

    fn is_unpinned(&self) -> bool {
        match self {
            Self::Owned(_) => true,
            Self::Shared(owner) => Arc::strong_count(owner) == 1,
            Self::Vacant => false,
        }
    }

    fn attach_ledger(&mut self, ledger: &Arc<ResidencyLedger>) {
        match self {
            Self::Owned(owner) => owner.ledger = Some(Arc::downgrade(ledger)),
            Self::Shared(owner) => {
                // A shared owner is normally created only after `ensure_ledger`.
                // Keep the fallback valid if a sole Arc is ever installed here.
                if let Some(owner) = Arc::get_mut(owner) {
                    owner.ledger = Some(Arc::downgrade(ledger));
                } else {
                    debug_assert!(false, "shared owner predates its residency ledger");
                }
            }
            Self::Vacant => {}
        }
    }

    fn lease(&mut self) -> Option<Arc<SegmentOwner>> {
        let previous = std::mem::replace(self, Self::Vacant);
        match previous {
            Self::Owned(owner) => {
                let shared = Arc::from(owner);
                let lease = Arc::clone(&shared);
                *self = Self::Shared(shared);
                Some(lease)
            }
            Self::Shared(owner) => {
                let lease = Arc::clone(&owner);
                *self = Self::Shared(owner);
                Some(lease)
            }
            Self::Vacant => None,
        }
    }
}

struct HotEntry {
    owner: HotOwner,
    pin: Option<Weak<PinnedOwnerGuard>>,
    last_used: u64,
}

#[derive(Clone, Copy, Default)]
struct ColdCost {
    pristine_samples: u32,
    delta_samples: u32,
    mean_pristine_ns: u64,
    mean_delta_ns: u64,
}

#[derive(Clone, Copy)]
enum RouteMode {
    Scanned,
    Prepared,
}

impl ColdCost {
    fn sample_count(self) -> u64 {
        u64::from(self.pristine_samples) + u64::from(self.delta_samples)
    }

    fn weighted_ns(self) -> u128 {
        u128::from(self.mean_pristine_ns) * u128::from(self.pristine_samples)
            + u128::from(self.mean_delta_ns) * u128::from(self.delta_samples)
    }

    fn cheapest_ns(self) -> Option<u64> {
        match (self.pristine_samples, self.delta_samples) {
            (0, 0) => None,
            (0, _) => Some(self.mean_delta_ns),
            (_, 0) => Some(self.mean_pristine_ns),
            (_, _) => Some(self.mean_pristine_ns.min(self.mean_delta_ns)),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Observation {
    uses: u32,
    // Prepared lookups and per-read action scans have different costs. Keep
    // their route choices independent while sharing one hot owner and budget.
    scanned: ColdCost,
    prepared: ColdCost,
    mean_hot_ns: u64,
    last_used: u64,
}

impl Observation {
    fn cost(self, mode: RouteMode) -> ColdCost {
        match mode {
            RouteMode::Scanned => self.scanned,
            RouteMode::Prepared => self.prepared,
        }
    }

    fn cost_mut(&mut self, mode: RouteMode) -> &mut ColdCost {
        match mode {
            RouteMode::Scanned => &mut self.scanned,
            RouteMode::Prepared => &mut self.prepared,
        }
    }
}

/// Single-owner, bounded policy for hot segment bytes, exact CAS fallback,
/// and short semantic-delta reuse routes.
///
/// Mutations require `&mut self`; read consumers can use [`Self::with_segment`]
/// to borrow bytes directly. Use [`Self::lease_hot_segment`] only when an
/// owner must outlive the cache borrow or cross to another reader thread.
pub struct AdaptiveIrResidency {
    limits: IrResidencyLimits,
    hot: HashMap<SegmentLookupKey, HotEntry>,
    observations: HashMap<SegmentLookupKey, Observation>,
    hot_bytes: u64,
    ledger: Option<Arc<ResidencyLedger>>,
    tick: u64,
    metrics: IrResidencyMetrics,
}

impl fmt::Debug for AdaptiveIrResidency {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdaptiveIrResidency")
            .field("limits", &self.limits)
            .field("hot_entries", &self.hot.len())
            .field("observations", &self.observations.len())
            .field("metrics", &self.metrics())
            .finish()
    }
}

impl Default for AdaptiveIrResidency {
    fn default() -> Self {
        Self::new(IrResidencyLimits::default())
    }
}

impl AdaptiveIrResidency {
    /// Creates an empty policy with explicit byte, entry, and delta bounds.
    #[must_use]
    pub fn new(limits: IrResidencyLimits) -> Self {
        Self {
            limits,
            hot: HashMap::new(),
            observations: HashMap::new(),
            hot_bytes: 0,
            ledger: None,
            tick: 0,
            metrics: IrResidencyMetrics::default(),
        }
    }

    /// Returns a stable snapshot of policy counters and accounted payload bytes.
    #[must_use]
    pub fn metrics(&self) -> IrResidencyMetrics {
        let mut metrics = self.metrics;
        metrics.hot_bytes = self.hot_bytes;
        metrics.live_owner_bytes = self.ledger.as_ref().map_or(self.hot_bytes, |ledger| {
            ledger.live_owner_bytes.load(Ordering::Relaxed)
        });
        metrics.pinned_owner_bytes = self.ledger.as_ref().map_or(0, |ledger| {
            ledger.pinned_owner_bytes.load(Ordering::Relaxed)
        });
        metrics
    }

    /// Validates a bounded manifest chain once for reuse across exact segment
    /// reads from `target`.
    ///
    /// Every later read still checks its selected authority and exact target
    /// descriptor. The returned plan borrows the final base and target
    /// manifests for candidate lookup, and caches only reusable descriptor
    /// indices.
    pub fn prepare_delta_route<'manifest>(
        &mut self,
        selection: SelectedSemanticPlane,
        target: &'manifest SemanticPlaneManifest,
        chain: &[IrResidencyDeltaHop<'manifest>],
    ) -> PreparedIrResidencyDeltaRoute<'manifest> {
        let started = Instant::now();
        let target_segments = target
            .plane(selection.kind())
            .map_or(&[][..], |plane| plane.segments());
        let mut reused_target_segment_indices = Vec::new();
        let evaluation = evaluate_delta_chain_into(
            selection,
            target,
            None,
            chain,
            self.limits,
            target_segments,
            Some(&mut reused_target_segment_indices),
        );
        if let DeltaEvaluation::Eligible(summary) = evaluation {
            self.record_delta_plan(summary, nanos(started.elapsed()));
        }
        PreparedIrResidencyDeltaRoute {
            kind: selection.kind(),
            target_root: target.root(),
            final_hop: chain.last().copied(),
            evaluation,
            target_segments,
            reused_target_segment_indices,
        }
    }

    /// Plans the next segment source for a freshly selected owner binding.
    ///
    /// Invalid or over-budget delta claims select the pristine target CAS path.
    /// The method never derives content identity from the semantic generation.
    pub fn plan<A: SelectedGenerationSource>(
        &self,
        source: &mut A,
        selection: SelectedSemanticPlane,
        manifest: &SemanticPlaneManifest,
        segment: &SemanticPlaneSegment,
        delta_chain: &[IrResidencyDeltaHop<'_>],
    ) -> Result<IrResidencyPath, IrResidencyError> {
        validate_target(selection, manifest, segment)?;
        ensure_current(source, selection)?;
        let key = SegmentLookupKey::from_descriptor(selection.kind(), segment);
        if self.hot_owner_matches(key, segment) {
            return Ok(IrResidencyPath::HotMemory);
        }
        let started = Instant::now();
        let evaluation =
            evaluate_delta_chain(selection, manifest, segment, delta_chain, self.limits);
        let planning_ns = nanos(started.elapsed());
        Ok(match evaluation {
            DeltaEvaluation::Eligible(summary) if summary.target_segment_reused => {
                let choice = self.delta_choice(
                    key,
                    summary,
                    planning_ns,
                    segment.byte_length(),
                    RouteMode::Scanned,
                );
                if choice == DeltaChoice::Use {
                    IrResidencyPath::DeltaCas(summary)
                } else {
                    IrResidencyPath::PristineCas(choice.cas_reason())
                }
            }
            DeltaEvaluation::Eligible(_) => {
                IrResidencyPath::PristineCas(IrResidencyCasReason::SegmentChanged)
            }
            DeltaEvaluation::Rejected(DeltaReject::NoRoute) => {
                IrResidencyPath::PristineCas(IrResidencyCasReason::NoDeltaRoute)
            }
            DeltaEvaluation::Rejected(DeltaReject::Bound) => {
                IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaBoundExceeded)
            }
            DeltaEvaluation::Rejected(DeltaReject::Claim) => {
                IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaRejected)
            }
        })
    }

    /// Plans one exact segment using a previously prepared delta route.
    ///
    /// This performs the same fresh authority and target-manifest checks as
    /// [`Self::plan`], while resolving reuse by a bounded borrowed-descriptor
    /// lookup instead of rescanning every action in the route.
    pub fn plan_with_prepared_route<A: SelectedGenerationSource>(
        &self,
        source: &mut A,
        selection: SelectedSemanticPlane,
        manifest: &SemanticPlaneManifest,
        segment: &SemanticPlaneSegment,
        route: &PreparedIrResidencyDeltaRoute<'_>,
    ) -> Result<IrResidencyPath, IrResidencyError> {
        validate_target(selection, manifest, segment)?;
        ensure_current(source, selection)?;
        let key = SegmentLookupKey::from_descriptor(selection.kind(), segment);
        if self.hot_owner_matches(key, segment) {
            return Ok(IrResidencyPath::HotMemory);
        }
        let started = Instant::now();
        let evaluation = route.evaluate(selection, manifest, segment);
        let planning_ns = nanos(started.elapsed());
        Ok(match evaluation {
            DeltaEvaluation::Eligible(summary) if summary.target_segment_reused => {
                let choice = self.delta_choice(
                    key,
                    summary,
                    planning_ns,
                    segment.byte_length(),
                    RouteMode::Prepared,
                );
                if choice == DeltaChoice::Use {
                    IrResidencyPath::DeltaCas(summary)
                } else {
                    IrResidencyPath::PristineCas(choice.cas_reason())
                }
            }
            DeltaEvaluation::Eligible(_) => {
                IrResidencyPath::PristineCas(IrResidencyCasReason::SegmentChanged)
            }
            DeltaEvaluation::Rejected(DeltaReject::NoRoute) => {
                IrResidencyPath::PristineCas(IrResidencyCasReason::NoDeltaRoute)
            }
            DeltaEvaluation::Rejected(DeltaReject::Bound) => {
                IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaBoundExceeded)
            }
            DeltaEvaluation::Rejected(DeltaReject::Claim) => {
                IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaRejected)
            }
        })
    }

    /// Reads one segment through a borrowed callback, choosing hot memory,
    /// checked delta CAS reuse, or the exact target CAS fallback.
    ///
    /// The callback runs only after the selected stamp and image membership
    /// have been freshly rechecked and the payload has matched the exact
    /// manifest descriptor. Its result must not be used to claim a composite
    /// semantic reader.
    pub fn with_segment<A, C, R, F>(
        &mut self,
        source: &mut A,
        selection: SelectedSemanticPlane,
        manifest: &SemanticPlaneManifest,
        segment: &SemanticPlaneSegment,
        delta_chain: &[IrResidencyDeltaHop<'_>],
        store: &mut C,
        read: F,
    ) -> Result<(IrResidencyPath, R), IrResidencyError>
    where
        A: SelectedGenerationSource,
        C: DurableSemanticRangeStore,
        F: FnOnce(&[u8]) -> R,
    {
        self.with_segment_using_route(
            source,
            selection,
            manifest,
            segment,
            DeltaRouteRef::Chain(delta_chain),
            store,
            read,
        )
    }

    /// Reads one segment using a previously prepared route.
    ///
    /// The target authority and exact segment descriptor are still checked
    /// for each call. Route actions are not rescanned; reuse is answered from
    /// the prepared route's bounded descriptor index.
    pub fn with_segment_prepared<A, C, R, F>(
        &mut self,
        source: &mut A,
        selection: SelectedSemanticPlane,
        manifest: &SemanticPlaneManifest,
        segment: &SemanticPlaneSegment,
        route: &PreparedIrResidencyDeltaRoute<'_>,
        store: &mut C,
        read: F,
    ) -> Result<(IrResidencyPath, R), IrResidencyError>
    where
        A: SelectedGenerationSource,
        C: DurableSemanticRangeStore,
        F: FnOnce(&[u8]) -> R,
    {
        self.with_segment_using_route(
            source,
            selection,
            manifest,
            segment,
            DeltaRouteRef::Prepared(route),
            store,
            read,
        )
    }

    fn with_segment_using_route<A, C, R, F>(
        &mut self,
        source: &mut A,
        selection: SelectedSemanticPlane,
        manifest: &SemanticPlaneManifest,
        segment: &SemanticPlaneSegment,
        route: DeltaRouteRef<'_, '_>,
        store: &mut C,
        read: F,
    ) -> Result<(IrResidencyPath, R), IrResidencyError>
    where
        A: SelectedGenerationSource,
        C: DurableSemanticRangeStore,
        F: FnOnce(&[u8]) -> R,
    {
        let started = Instant::now();
        validate_target(selection, manifest, segment)?;
        let key = SegmentLookupKey::from_descriptor(selection.kind(), segment);

        if self.hot.contains_key(&key) {
            let valid = self.hot_owner_matches(key, segment);
            if valid {
                self.touch_hot(key);
                ensure_current(source, selection)?;
                self.observe_hot(key, nanos(started.elapsed()));
                Self::increment(&mut self.metrics.hot_hits);
                Self::increment(&mut self.metrics.borrowed_reads);
                let payload = self
                    .hot
                    .get(&key)
                    .and_then(|entry| entry.owner.bytes())
                    .ok_or(IrResidencyError::CorruptHotOwner)?;
                return Ok((IrResidencyPath::HotMemory, read(payload)));
            }
            self.remove_hot(key);
            Self::increment(&mut self.metrics.corrupt_hot_owners);
        }

        let planning_started = Instant::now();
        let evaluation = route.evaluate(selection, manifest, segment, self.limits);
        let planning_ns = nanos(planning_started.elapsed());
        if route.record_plan_for_read() {
            if let DeltaEvaluation::Eligible(summary) = evaluation {
                self.record_delta_plan(summary, planning_ns);
            }
        }
        let (path, payload, id) = match evaluation {
            DeltaEvaluation::Eligible(summary) if summary.target_segment_reused => {
                let choice = self.delta_choice(
                    key,
                    summary,
                    planning_ns,
                    segment.byte_length(),
                    route.mode(),
                );
                if choice == DeltaChoice::Use {
                    if let Some((base_selection, request)) =
                        delta_candidate(selection.kind(), segment, route.final_hop())
                    {
                        match self.read_candidate(
                            source,
                            base_selection,
                            request,
                            selection,
                            segment,
                            store,
                        )? {
                            Some((bytes, id)) => {
                                Self::increment(&mut self.metrics.delta_reads);
                                (IrResidencyPath::DeltaCas(summary), bytes, id)
                            }
                            None => {
                                Self::increment(&mut self.metrics.delta_rejections);
                                let (bytes, id) = self.read_target_request(
                                    source,
                                    selection,
                                    request_for(selection, manifest, segment),
                                    segment,
                                    store,
                                )?;
                                (
                                    IrResidencyPath::PristineCas(
                                        IrResidencyCasReason::DeltaCandidateUnavailable,
                                    ),
                                    bytes,
                                    id,
                                )
                            }
                        }
                    } else {
                        Self::increment(&mut self.metrics.delta_rejections);
                        let (bytes, id) = self.read_target_request(
                            source,
                            selection,
                            request_for(selection, manifest, segment),
                            segment,
                            store,
                        )?;
                        (
                            IrResidencyPath::PristineCas(IrResidencyCasReason::DeltaRejected),
                            bytes,
                            id,
                        )
                    }
                } else {
                    self.read_pristine_with_reason(
                        source,
                        selection,
                        manifest,
                        segment,
                        store,
                        choice.cas_reason(),
                    )?
                }
            }
            DeltaEvaluation::Eligible(_) => self.read_pristine_with_reason(
                source,
                selection,
                manifest,
                segment,
                store,
                IrResidencyCasReason::SegmentChanged,
            )?,
            DeltaEvaluation::Rejected(DeltaReject::NoRoute) => self.read_pristine_with_reason(
                source,
                selection,
                manifest,
                segment,
                store,
                IrResidencyCasReason::NoDeltaRoute,
            )?,
            DeltaEvaluation::Rejected(DeltaReject::Bound) => self.read_pristine_with_reason(
                source,
                selection,
                manifest,
                segment,
                store,
                IrResidencyCasReason::DeltaBoundExceeded,
            )?,
            DeltaEvaluation::Rejected(DeltaReject::Claim) => {
                Self::increment(&mut self.metrics.delta_rejections);
                self.read_pristine_with_reason(
                    source,
                    selection,
                    manifest,
                    segment,
                    store,
                    IrResidencyCasReason::DeltaRejected,
                )?
            }
        };

        ensure_current(source, selection)?;
        let elapsed = nanos(started.elapsed());
        self.observe_cold(
            key,
            elapsed,
            matches!(path, IrResidencyPath::DeltaCas(_)),
            route.mode(),
        );
        let transient = self.consider_hot(key, id, payload);
        Self::increment(&mut self.metrics.borrowed_reads);

        if let Some(entry) = self.hot.get(&key) {
            let payload = entry
                .owner
                .bytes()
                .ok_or(IrResidencyError::CorruptHotOwner)?;
            return Ok((path, read(payload)));
        }
        let payload = transient
            .as_deref()
            .ok_or(IrResidencyError::CorruptHotOwner)?;
        Ok((path, read(payload)))
    }

    /// Creates a lease for one cached segment when it is already hot.
    ///
    /// The lease owns an `Arc` so the caller can retain bytes beyond this
    /// mutable cache borrow. Its bytes remain private; every exposure checks
    /// the exact captured selected stamp again through the live authority.
    pub fn lease_hot_segment<A: SelectedGenerationSource>(
        &mut self,
        source: &mut A,
        selection: SelectedSemanticPlane,
        manifest: &SemanticPlaneManifest,
        segment: &SemanticPlaneSegment,
    ) -> Result<Option<IrResidencyLease>, IrResidencyError> {
        validate_target(selection, manifest, segment)?;
        ensure_current(source, selection)?;
        let key = SegmentLookupKey::from_descriptor(selection.kind(), segment);
        let byte_length = {
            let Some(entry) = self.hot.get(&key) else {
                return Ok(None);
            };
            let Some(bytes) = entry.owner.bytes() else {
                return Err(IrResidencyError::CorruptHotOwner);
            };
            if !entry.owner.id().is_some_and(|id| {
                id.as_bytes() == segment.id_claim().as_bytes()
                    && bytes.len() as u64 == segment.byte_length()
            }) {
                return Err(IrResidencyError::CorruptHotOwner);
            }
            bytes.len() as u64
        };
        let existing_pin = self
            .hot
            .get(&key)
            .and_then(|entry| entry.pin.as_ref())
            .and_then(Weak::upgrade);
        if existing_pin.is_none() {
            let pinned = self.ledger.as_ref().map_or(0, |ledger| {
                ledger.pinned_owner_bytes.load(Ordering::Relaxed)
            });
            if pinned
                .checked_add(byte_length)
                .is_none_or(|total| total > self.limits.pinned_bytes)
            {
                return Err(IrResidencyError::PinnedBudgetExceeded);
            }
        }
        let ledger = self.ensure_ledger();
        let pin = {
            let entry = self
                .hot
                .get_mut(&key)
                .ok_or(IrResidencyError::CorruptHotOwner)?;
            let pin = existing_pin.or_else(|| entry.pin.as_ref().and_then(Weak::upgrade));
            if let Some(pin) = pin {
                pin
            } else {
                ledger
                    .pinned_owner_bytes
                    .fetch_add(byte_length, Ordering::Relaxed);
                let pin = Arc::new(PinnedOwnerGuard {
                    bytes: byte_length,
                    ledger: Arc::clone(&ledger),
                });
                entry.pin = Some(Arc::downgrade(&pin));
                pin
            }
        };
        let owner = self
            .hot
            .get_mut(&key)
            .and_then(|entry| entry.owner.lease())
            .ok_or(IrResidencyError::CorruptHotOwner)?;
        self.touch_hot(key);
        ensure_current(source, selection)?;
        Self::increment(&mut self.metrics.leases_issued);
        Ok(Some(IrResidencyLease {
            owner,
            selection,
            _pin: pin,
        }))
    }

    fn read_pristine_with_reason<A, C>(
        &mut self,
        source: &mut A,
        selection: SelectedSemanticPlane,
        manifest: &SemanticPlaneManifest,
        segment: &SemanticPlaneSegment,
        store: &mut C,
        reason: IrResidencyCasReason,
    ) -> Result<(IrResidencyPath, Box<[u8]>, SemanticSegmentId), IrResidencyError>
    where
        A: SelectedGenerationSource,
        C: DurableSemanticRangeStore,
    {
        let request = request_for(selection, manifest, segment);
        self.read_target_request(source, selection, request, segment, store)
            .map(|(payload, id)| (IrResidencyPath::PristineCas(reason), payload, id))
    }

    fn read_candidate<A, C>(
        &mut self,
        source: &mut A,
        candidate_selection: SelectedSemanticPlane,
        candidate_request: SemanticRangeRequest,
        target_selection: SelectedSemanticPlane,
        target_segment: &SemanticPlaneSegment,
        store: &mut C,
    ) -> Result<Option<(Box<[u8]>, SemanticSegmentId)>, IrResidencyError>
    where
        A: SelectedGenerationSource,
        C: DurableSemanticRangeStore,
    {
        ensure_current(source, target_selection)?;
        Self::increment(&mut self.metrics.cold_reads);
        let payload = match store.read_complete_segment(candidate_selection, candidate_request) {
            Ok(payload) => payload,
            Err(_) => {
                // A historical object is only an optimization. The selected
                // target still has an independent exact CAS commitment and
                // remains the authoritative fallback. Never expose candidate
                // bytes or skip the final current-selection check.
                Self::increment(&mut self.metrics.unreadable_delta_candidates);
                return Ok(None);
            }
        };
        let Some(payload) = payload else {
            return Ok(None);
        };
        let id = match target_segment.admit(target_selection.kind(), &payload) {
            Ok(id) => id,
            Err(_) => {
                Self::increment(&mut self.metrics.corrupt_cas_objects);
                return Ok(None);
            }
        };
        Ok(Some((payload, id)))
    }

    fn read_target_request<A, C>(
        &mut self,
        source: &mut A,
        selection: SelectedSemanticPlane,
        request: SemanticRangeRequest,
        segment: &SemanticPlaneSegment,
        store: &mut C,
    ) -> Result<(Box<[u8]>, SemanticSegmentId), IrResidencyError>
    where
        A: SelectedGenerationSource,
        C: DurableSemanticRangeStore,
    {
        ensure_current(source, selection)?;
        Self::increment(&mut self.metrics.cold_reads);
        let payload = store
            .read_complete_segment(selection, request)
            .map_err(|error| IrResidencyError::Storage(error.to_string()))?;
        let payload = payload.ok_or(IrResidencyError::MissingSegment)?;
        let id = segment.admit(selection.kind(), &payload).map_err(|_| {
            Self::increment(&mut self.metrics.corrupt_cas_objects);
            IrResidencyError::CorruptCas
        })?;
        Ok((payload, id))
    }

    fn consider_hot(
        &mut self,
        key: SegmentLookupKey,
        id: SemanticSegmentId,
        payload: Box<[u8]>,
    ) -> Option<Box<[u8]>> {
        let size = payload.len() as u64;
        if !self.should_warm(key) {
            return Some(payload);
        }
        if size > self.limits.hot_bytes || self.limits.entries == 0 {
            Self::increment(&mut self.metrics.admission_rejections);
            return Some(payload);
        }
        if !self.make_room(size) {
            Self::increment(&mut self.metrics.admission_rejections);
            return Some(payload);
        }
        while self.hot.len() >= self.limits.entries {
            if !self.evict_one(false) {
                Self::increment(&mut self.metrics.admission_rejections);
                return Some(payload);
            }
        }
        let owner = SegmentOwner::new(id, payload, self.ledger.as_ref());
        self.hot_bytes = self.hot_bytes.saturating_add(size);
        let last_used = self.next_tick();
        self.hot.insert(
            key,
            HotEntry {
                owner: HotOwner::Owned(owner),
                pin: None,
                last_used,
            },
        );
        Self::increment(&mut self.metrics.hot_admissions);
        None
    }

    fn should_warm(&self, key: SegmentLookupKey) -> bool {
        let observation = self.observations.get(&key).copied().unwrap_or_default();
        if observation.uses < self.limits.warm_uses {
            return false;
        }
        let cold_samples = observation.scanned.sample_count() + observation.prepared.sample_count();
        if cold_samples == 0 {
            return false;
        }
        let weighted_cold_ns = (observation.scanned.weighted_ns()
            + observation.prepared.weighted_ns())
            / u128::from(cold_samples);
        weighted_cold_ns >= u128::from(duration_ns(self.limits.warm_cold_cost))
    }

    fn delta_choice(
        &self,
        key: SegmentLookupKey,
        summary: IrResidencyDeltaSummary,
        planning_ns: u64,
        segment_bytes: u64,
        mode: RouteMode,
    ) -> DeltaChoice {
        let observation = self.observations.get(&key).copied().unwrap_or_default();
        let cost = observation.cost(mode);
        let modest_route = summary.hops <= 2
            && summary.actions <= self.limits.delta_actions.min(512)
            && summary.changed_bytes <= segment_bytes.saturating_mul(4);
        if !modest_route {
            return DeltaChoice::FanoutHigh;
        }
        match (cost.delta_samples, cost.pristine_samples) {
            (0, 0) => DeltaChoice::Use,
            (0, _) => {
                if planning_ns.saturating_mul(4) < cost.mean_pristine_ns.max(1) {
                    DeltaChoice::Use
                } else {
                    DeltaChoice::CostHigher
                }
            }
            (_, 0) => DeltaChoice::BaselineProbe,
            (_, _) => {
                // Observations span planning, verification, and the final
                // authority check, so adding planning again would bias the
                // route toward pristine CAS after the first sample.
                let delta_cost = cost.mean_delta_ns;
                let pristine_cost = cost.mean_pristine_ns;
                let cold_samples = cost.delta_samples.saturating_add(cost.pristine_samples);
                if delta_cost.saturating_mul(110) < pristine_cost.saturating_mul(100)
                    || (cold_samples >= DELTA_REPROBE_INTERVAL
                        && cold_samples % DELTA_REPROBE_INTERVAL == 0)
                {
                    DeltaChoice::Use
                } else {
                    DeltaChoice::CostHigher
                }
            }
        }
    }

    fn record_delta_plan(&mut self, summary: IrResidencyDeltaSummary, planning_ns: u64) {
        Self::add(&mut self.metrics.delta_plan_ns, planning_ns);
        Self::add(
            &mut self.metrics.delta_actions_scanned,
            summary.actions as u64,
        );
        Self::add(
            &mut self.metrics.delta_changed_bytes_scanned,
            summary.changed_bytes,
        );
    }

    fn hot_owner_matches(&self, key: SegmentLookupKey, segment: &SemanticPlaneSegment) -> bool {
        self.hot.get(&key).is_some_and(|entry| {
            entry.owner.bytes().is_some_and(|bytes| {
                entry.owner.id().is_some_and(|id| {
                    id.as_bytes() == segment.id_claim().as_bytes()
                        && bytes.len() as u64 == segment.byte_length()
                })
            })
        })
    }

    fn make_room(&mut self, requested: u64) -> bool {
        loop {
            let live = self.ledger.as_ref().map_or(self.hot_bytes, |ledger| {
                ledger.live_owner_bytes.load(Ordering::Relaxed)
            });
            if live
                .checked_add(requested)
                .is_some_and(|total| total <= self.limits.hot_bytes)
            {
                return true;
            }
            if !self.evict_one(true) {
                return false;
            }
        }
    }

    fn ensure_ledger(&mut self) -> Arc<ResidencyLedger> {
        if let Some(ledger) = self.ledger.as_ref() {
            return Arc::clone(ledger);
        }
        let ledger = Arc::new(ResidencyLedger {
            live_owner_bytes: AtomicU64::new(self.hot_bytes),
            pinned_owner_bytes: AtomicU64::new(0),
        });
        for entry in self.hot.values_mut() {
            entry.owner.attach_ledger(&ledger);
        }
        self.ledger = Some(Arc::clone(&ledger));
        ledger
    }

    fn evict_one(&mut self, only_unpinned: bool) -> bool {
        let victim = self
            .hot
            .iter()
            .filter(|(_, entry)| !only_unpinned || entry.owner.is_unpinned())
            .min_by_key(|(key, entry)| {
                let bytes = entry.owner.bytes().map_or(0, <[u8]>::len);
                (self.utility(**key, bytes), entry.last_used)
            })
            .map(|(key, _)| *key);
        victim.is_some_and(|key| self.remove_hot(key))
    }

    fn utility(&self, key: SegmentLookupKey, bytes: usize) -> u64 {
        let observation = self.observations.get(&key).copied().unwrap_or_default();
        let fallback_ns = observation
            .scanned
            .cheapest_ns()
            .into_iter()
            .chain(observation.prepared.cheapest_ns())
            .min()
            .unwrap_or(0);
        let saved = fallback_ns.saturating_sub(observation.mean_hot_ns).max(1);
        u64::from(observation.uses).saturating_mul(saved)
            / u64::try_from(bytes.max(1)).unwrap_or(u64::MAX)
    }

    fn remove_hot(&mut self, key: SegmentLookupKey) -> bool {
        let Some(entry) = self.hot.remove(&key) else {
            return false;
        };
        let size = entry.owner.bytes().map_or(0, <[u8]>::len) as u64;
        self.hot_bytes = self.hot_bytes.saturating_sub(size);
        drop(entry);
        Self::increment(&mut self.metrics.evictions);
        true
    }

    fn touch_hot(&mut self, key: SegmentLookupKey) {
        let tick = self.next_tick();
        if let Some(entry) = self.hot.get_mut(&key) {
            entry.last_used = tick;
        }
    }

    fn observe_hot(&mut self, key: SegmentLookupKey, elapsed: u64) {
        if self.limits.entries == 0 {
            return;
        }
        let tick = self.next_tick();
        let observation = self.observation(key, tick);
        observation.uses = observation.uses.saturating_add(1);
        observation.mean_hot_ns = mean(observation.mean_hot_ns, elapsed);
    }

    fn observe_cold(
        &mut self,
        key: SegmentLookupKey,
        elapsed: u64,
        was_delta: bool,
        mode: RouteMode,
    ) {
        if self.limits.entries == 0 {
            return;
        }
        let tick = self.next_tick();
        let observation = self.observation(key, tick);
        observation.uses = observation.uses.saturating_add(1);
        let cost = observation.cost_mut(mode);
        if was_delta {
            cost.delta_samples = cost.delta_samples.saturating_add(1);
            cost.mean_delta_ns = mean(cost.mean_delta_ns, elapsed);
        } else {
            cost.pristine_samples = cost.pristine_samples.saturating_add(1);
            cost.mean_pristine_ns = mean(cost.mean_pristine_ns, elapsed);
        }
    }

    fn observation(&mut self, key: SegmentLookupKey, tick: u64) -> &mut Observation {
        if !self.observations.contains_key(&key) && self.observations.len() >= self.limits.entries {
            if let Some(oldest) = self
                .observations
                .iter()
                .min_by_key(|(_, item)| item.last_used)
                .map(|(key, _)| *key)
            {
                self.observations.remove(&oldest);
            }
        }
        let observation = self.observations.entry(key).or_default();
        observation.last_used = tick;
        observation
    }

    fn next_tick(&mut self) -> u64 {
        self.tick = self.tick.saturating_add(1);
        self.tick
    }

    fn increment(counter: &mut u64) {
        *counter = counter.saturating_add(1);
    }

    fn add(counter: &mut u64, amount: u64) {
        *counter = counter.saturating_add(amount);
    }
}

/// Owned lease for bytes that must outlive the mutable policy borrow.
pub struct IrResidencyLease {
    owner: Arc<SegmentOwner>,
    selection: SelectedSemanticPlane,
    _pin: Arc<PinnedOwnerGuard>,
}

impl IrResidencyLease {
    /// Admitted content identity, independent from the selected authority binding.
    #[must_use]
    pub fn content_id(&self) -> SemanticSegmentId {
        self.owner.id
    }

    /// Authority binding captured when this lease was issued.
    #[must_use]
    pub const fn selection(&self) -> SelectedSemanticPlane {
        self.selection
    }

    /// Borrows bytes only after freshly confirming the captured owner binding.
    pub fn with_payload<A, R>(
        &self,
        source: &mut A,
        read: impl FnOnce(&[u8]) -> R,
    ) -> Result<R, IrResidencyError>
    where
        A: SelectedGenerationSource,
    {
        ensure_current(source, self.selection)?;
        Ok(read(&self.owner.bytes))
    }
}

impl Clone for IrResidencyLease {
    fn clone(&self) -> Self {
        Self {
            owner: Arc::clone(&self.owner),
            selection: self.selection,
            _pin: Arc::clone(&self._pin),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeltaReject {
    NoRoute,
    Bound,
    Claim,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeltaChoice {
    Use,
    BaselineProbe,
    CostHigher,
    FanoutHigh,
}

impl DeltaChoice {
    const fn cas_reason(self) -> IrResidencyCasReason {
        match self {
            Self::Use => IrResidencyCasReason::NoDeltaRoute,
            Self::BaselineProbe => IrResidencyCasReason::DeltaBaselineProbe,
            Self::CostHigher => IrResidencyCasReason::DeltaCostHigher,
            Self::FanoutHigh => IrResidencyCasReason::DeltaFanoutHigh,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeltaEvaluation {
    Eligible(IrResidencyDeltaSummary),
    Rejected(DeltaReject),
}

#[derive(Clone, Copy)]
enum DeltaRouteRef<'route, 'manifest> {
    Chain(&'route [IrResidencyDeltaHop<'manifest>]),
    Prepared(&'route PreparedIrResidencyDeltaRoute<'manifest>),
}

impl<'route, 'manifest> DeltaRouteRef<'route, 'manifest> {
    fn evaluate(
        self,
        selection: SelectedSemanticPlane,
        target: &SemanticPlaneManifest,
        requested: &SemanticPlaneSegment,
        limits: IrResidencyLimits,
    ) -> DeltaEvaluation {
        match self {
            Self::Chain(chain) => evaluate_delta_chain(selection, target, requested, chain, limits),
            Self::Prepared(route) => route.evaluate(selection, target, requested),
        }
    }

    fn final_hop(self) -> Option<IrResidencyDeltaHop<'manifest>> {
        match self {
            Self::Chain(chain) => chain.last().copied(),
            Self::Prepared(route) => route.final_hop,
        }
    }

    const fn record_plan_for_read(self) -> bool {
        matches!(self, Self::Chain(_))
    }

    const fn mode(self) -> RouteMode {
        match self {
            Self::Chain(_) => RouteMode::Scanned,
            Self::Prepared(_) => RouteMode::Prepared,
        }
    }
}

fn evaluate_delta_chain(
    selection: SelectedSemanticPlane,
    target: &SemanticPlaneManifest,
    requested: &SemanticPlaneSegment,
    chain: &[IrResidencyDeltaHop<'_>],
    limits: IrResidencyLimits,
) -> DeltaEvaluation {
    evaluate_delta_chain_into(selection, target, Some(requested), chain, limits, &[], None)
}

fn evaluate_delta_chain_into(
    selection: SelectedSemanticPlane,
    target: &SemanticPlaneManifest,
    requested: Option<&SemanticPlaneSegment>,
    chain: &[IrResidencyDeltaHop<'_>],
    limits: IrResidencyLimits,
    target_segments: &[SemanticPlaneSegment],
    mut reused_target_segment_indices: Option<&mut Vec<usize>>,
) -> DeltaEvaluation {
    if chain.is_empty() {
        return DeltaEvaluation::Rejected(DeltaReject::NoRoute);
    }
    if chain.len() > limits.delta_hops {
        return DeltaEvaluation::Rejected(DeltaReject::Bound);
    }
    if chain
        .last()
        .is_none_or(|hop| hop.target.root() != target.root())
    {
        return DeltaEvaluation::Rejected(DeltaReject::Claim);
    }
    let mut previous_root = None;
    let mut actions = 0_usize;
    let mut reused = 0_usize;
    let mut changed_bytes = 0_u64;
    let mut final_reused = false;
    let mut final_target_position = 0_usize;
    for (hop_index, hop) in chain.iter().enumerate() {
        if previous_root.is_some_and(|root| root != hop.base.root())
            || hop.base.root() != hop.expected_base_root
            || hop.base_selection.kind() != selection.kind()
            || !selection_matches_manifest(hop.base_selection, hop.base)
            || !delta_claims_complete(hop.base, selection.kind())
            || !delta_claims_complete(hop.target, selection.kind())
        {
            return DeltaEvaluation::Rejected(DeltaReject::Claim);
        }
        let mut cursor =
            match SemanticDeltaCursor::new(hop.base, hop.target, hop.expected_base_root) {
                Ok(cursor) => cursor,
                Err(_) => return DeltaEvaluation::Rejected(DeltaReject::Claim),
            };
        loop {
            let Some(action) = cursor.next_action() else {
                break;
            };
            actions = actions.saturating_add(1);
            if actions > limits.delta_actions {
                return DeltaEvaluation::Rejected(DeltaReject::Bound);
            }
            match action {
                SemanticDeltaAction::Reuse {
                    plane,
                    segment_id,
                    segment,
                } if plane == selection.kind() => {
                    reused = reused.saturating_add(1);
                    if hop_index + 1 == chain.len() {
                        if requested.is_some_and(|requested| {
                            segment_id.as_bytes() == requested.id_claim().as_bytes()
                                && segment == *requested
                        }) {
                            final_reused = true;
                        }
                        if let Some(indices) = reused_target_segment_indices.as_deref_mut() {
                            if target_segments.get(final_target_position) != Some(&segment) {
                                return DeltaEvaluation::Rejected(DeltaReject::Claim);
                            }
                            // The cursor emits target actions in manifest order,
                            // so these indices stay sorted for later binary search.
                            indices.push(final_target_position);
                        }
                        final_target_position = final_target_position.saturating_add(1);
                    }
                }
                SemanticDeltaAction::Fetch(request) if request.plane == selection.kind() => {
                    changed_bytes = changed_bytes.saturating_add(request.byte_length);
                    if hop_index + 1 == chain.len() {
                        final_target_position = final_target_position.saturating_add(1);
                    }
                }
                SemanticDeltaAction::Remove { plane, segment } if plane == selection.kind() => {
                    changed_bytes = changed_bytes.saturating_add(segment.byte_length());
                }
                _ => {}
            }
            if changed_bytes > limits.delta_changed_bytes {
                return DeltaEvaluation::Rejected(DeltaReject::Bound);
            }
        }
        previous_root = Some(hop.target.root());
    }
    let summary = IrResidencyDeltaSummary {
        hops: chain.len(),
        actions,
        reused_segments: reused,
        changed_bytes,
        target_segment_reused: final_reused,
    };
    DeltaEvaluation::Eligible(summary)
}

fn delta_claims_complete(manifest: &SemanticPlaneManifest, kind: SemanticPlaneKind) -> bool {
    manifest.claims_admitted()
        && manifest.input().coverage().is_authorized_complete()
        && manifest
            .plane(kind)
            .is_some_and(|plane| plane.coverage().is_authorized_complete())
}

fn delta_candidate(
    kind: SemanticPlaneKind,
    requested: &SemanticPlaneSegment,
    hop: Option<IrResidencyDeltaHop<'_>>,
) -> Option<(SelectedSemanticPlane, SemanticRangeRequest)> {
    let hop = hop?;
    let base_plane = hop.base.plane(kind)?;
    let segments = base_plane.segments();
    let position =
        segments.partition_point(|candidate| candidate.first_key() < requested.first_key());
    let base_segment = segments.get(position).filter(|candidate| {
        candidate.id_claim() == requested.id_claim()
            && candidate.first_key() == requested.first_key()
            && candidate.last_key() == requested.last_key()
            && candidate.row_count() == requested.row_count()
            && candidate.byte_length() == requested.byte_length()
    })?;
    Some((
        hop.base_selection,
        request_for(hop.base_selection, hop.base, base_segment),
    ))
}

fn validate_target(
    selection: SelectedSemanticPlane,
    manifest: &SemanticPlaneManifest,
    segment: &SemanticPlaneSegment,
) -> Result<(), IrResidencyError> {
    if !selection_matches_manifest(selection, manifest) {
        return Err(IrResidencyError::SelectionManifestMismatch);
    }
    let plane = manifest
        .plane(selection.kind())
        .ok_or(IrResidencyError::SelectionManifestMismatch)?;
    let segments = plane.segments();
    let position =
        segments.partition_point(|candidate| candidate.first_key() < segment.first_key());
    if segments.get(position) != Some(segment) {
        return Err(IrResidencyError::SegmentNotInManifest);
    }
    Ok(())
}

fn selection_matches_manifest(
    selection: SelectedSemanticPlane,
    manifest: &SemanticPlaneManifest,
) -> bool {
    let image = selection.image();
    image.manifest_root() == manifest.root()
        && image.semantic_generation() == manifest.semantic_generation()
        && selection.stamp().profile() == manifest.build().profile()
        && manifest
            .plane(selection.kind())
            .is_some_and(|plane| plane.root() == selection.root())
}

fn ensure_current<A: SelectedGenerationSource>(
    source: &mut A,
    selection: SelectedSemanticPlane,
) -> Result<(), IrResidencyError> {
    let current = source
        .current_selected_generation()
        .map_err(|error| IrResidencyError::Frontier(error.to_string()))?;
    if current != selection.stamp()
        || !source
            .selected_image_is_current(current, selection.image())
            .map_err(|error| IrResidencyError::Frontier(error.to_string()))?
    {
        return Err(IrResidencyError::StaleSelection);
    }
    Ok(())
}

fn request_for(
    selection: SelectedSemanticPlane,
    manifest: &SemanticPlaneManifest,
    segment: &SemanticPlaneSegment,
) -> SemanticRangeRequest {
    SemanticRangeRequest {
        manifest_root: manifest.root(),
        plane: selection.kind(),
        segment_id: segment.id_claim(),
        first_key: *segment.first_key(),
        last_key: *segment.last_key(),
        byte_length: segment.byte_length(),
    }
}

fn mean(previous: u64, sample: u64) -> u64 {
    if previous == 0 {
        sample
    } else {
        previous - previous / 4 + sample / 4
    }
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn duration_ns(duration: Duration) -> u64 {
    nanos(duration)
}

#[cfg(test)]
mod tests;
