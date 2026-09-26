//! The `adaptive` module exists to choose local or remote execution from typed resource and consistency facts.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! A bounded, deterministic local-first placement policy.
//!
//! [`next_action`] is deliberately pure: it reads one immutable, caller-owned snapshot and emits
//! one typed placement decision. Adapters translate [`PlacementAction`] into their concrete async
//! vocabulary; this module neither starts I/O nor retains mutable execution state.

mod scalar;
mod policy;

pub use policy::next_action;

pub use backend_version::{
    CapabilityDomain, ContentId, GenerationId, IndexSnapshotId, ObjectDomain,
};
pub use scalar::{ByteCount, LatencyMicros, OperationBudget, RetryBudget};

/// Maximum local residence facts admitted by one policy snapshot.
pub const MAX_LOCAL_FACTS: usize = 8;
/// Maximum remote availability facts admitted by one policy snapshot.
pub const MAX_REMOTE_FACTS: usize = 8;
/// Maximum demand facts admitted by one policy snapshot.
pub const MAX_DEMAND_FACTS: usize = 8;
/// Maximum optional capability bundles admitted by one policy snapshot.
pub const MAX_BUNDLE_FACTS: usize = 8;

/// The generation and snapshot that every placement fact must carry.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Pin {
    /// Immutable generation authority.
    pub generation: GenerationId,
    /// Immutable snapshot authority within that generation.
    pub snapshot: IndexSnapshotId,
}

/// A canonical fact identity. Physical placement never changes this value.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FactKey {
    /// Immutable generation/snapshot authority.
    pub pin: Pin,
    /// Fact selected under that authority.
    pub object: ContentId<ObjectDomain>,
}

/// Physical residence of canonical bytes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, strum::AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum StorageTier {
    /// Process-addressable memory.
    Ram,
    /// Local durable storage.
    Nvme,
}

/// Authority that controls whether local residence may be removed.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Retention {
    /// No consumer prevents contraction.
    Idle,
    /// Product policy requires the fact to remain local.
    Pinned,
    /// A running local operation owns the fact.
    InUse,
}

/// One observed physical copy of canonical bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalFact {
    /// Canonical identity, pinned to the snapshot.
    pub key: FactKey,
    /// Current physical owner.
    pub tier: StorageTier,
    /// Exact retained bytes.
    pub bytes: ByteCount,
    /// Existing owner authority.
    pub retention: Retention,
}

/// A remotely available canonical fact, already pinned to its immutable authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteFact {
    /// Canonical identity, pinned to the snapshot.
    pub key: FactKey,
    /// Exact fetch reservation.
    pub bytes: ByteCount,
}

/// Current consumer demand for a fact.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DemandLevel {
    /// No admission-worthy consumer demand.
    Cold,
    /// Reuse is likely but not latency critical.
    Warm,
    /// A latency-critical consumer is waiting.
    Hot,
}

/// One demand fact observed by the product boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Demand {
    /// Requested canonical fact.
    pub key: FactKey,
    /// Closed demand class.
    pub level: DemandLevel,
    /// Largest acceptable remote latency.
    pub latency_budget: LatencyMicros,
}

/// Optional product capability supplied by a verified bundle.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, strum::AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum CapabilityKind {
    /// Language or semantic analyzer.
    Analyzer,
    /// Compiler frontend.
    Compiler,
    /// Codec adapter.
    Codec,
    /// Inference model.
    Model,
}

/// Externally observable physical residence of a verified capability bundle.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BundleResidence {
    /// Verified bytes are locally available but inactive.
    Available,
    /// The verified capability is active.
    Active,
}

/// Closed bundle state coupling residence to its current consumer authority.
///
/// In particular, an inactive bundle cannot be in use and an active bundle cannot still be
/// awaiting acquisition. Keeping those combinations out of the vocabulary prevents contraction
/// from releasing a live capability and prevents outages from activating unrelated bundles.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BundleState {
    /// Verified bytes are locally available and no consumer requires activation.
    AvailableIdle,
    /// A consumer requires this locally available capability.
    AvailableRequired,
    /// The capability is active but no running operation owns it.
    ActiveIdle,
    /// A running local operation owns the active capability.
    ActiveInUse,
}

impl BundleState {
    /// Return the externally observable physical residence.
    #[must_use]
    pub const fn residence(self) -> BundleResidence {
        match self {
            Self::AvailableIdle | Self::AvailableRequired => BundleResidence::Available,
            Self::ActiveIdle | Self::ActiveInUse => BundleResidence::Active,
        }
    }
}

/// One bundle fact from the signature-verification boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BundleFact {
    /// Typed optional capability.
    pub capability: CapabilityKind,
    /// Hash-pinned verified artifact.
    pub bundle: ContentId<CapabilityDomain>,
    /// Observable residence and consumer authority; the policy keeps no shadow state.
    pub state: BundleState,
}

/// Current remote health as observed by the adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteHealth {
    /// The remote reports a concrete generation/snapshot and measured latency.
    Healthy {
        /// Authority reported by the remote.
        observed: Pin,
        /// Measured round-trip latency.
        latency: LatencyMicros,
    },
    /// The remote cannot currently be reached.
    Outage,
    /// The remote answered under different immutable authority.
    Inconsistent {
        /// Authority actually reported by the remote.
        observed: Pin,
    },
}

/// Coarse platform battery policy.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BatteryState {
    /// External power is available.
    Charging,
    /// Ordinary demand-driven work is permitted.
    Normal,
    /// Only hot demand or recovery work is permitted.
    Low,
    /// No optional remote fetch starts.
    Critical,
}

/// Bounded physical-owner pressure.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Pressure {
    /// Capacity is comfortably inside its physical bound.
    Relaxed,
    /// Idle residence should contract when possible.
    Elevated,
    /// Idle residence must contract before lower-priority work.
    Critical,
}

/// Explicit budgets consumed by execution between policy snapshots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceBudget {
    /// Unreserved RAM bytes.
    pub ram_free: ByteCount,
    /// Unreserved local durable bytes.
    pub nvme_free: ByteCount,
    /// Independent actions admitted by the platform.
    pub operations: OperationBudget,
    /// Safe remote recovery attempts still admitted.
    pub retries: RetryBudget,
    /// Current RAM owner pressure.
    pub memory_pressure: Pressure,
    /// Current `NVMe` owner pressure.
    pub storage_pressure: Pressure,
    /// Battery policy from the platform adapter.
    pub battery: BatteryState,
}

/// Caller-owned immutable facts consumed by [`next_action`].
pub struct PolicyInput<'facts> {
    /// Authority pinned by the consumer for this decision.
    pub pin: Pin,
    /// Current local physical residences.
    pub local: &'facts [LocalFact],
    /// Remote, pin-bearing availability facts.
    pub remote: &'facts [RemoteFact],
    /// Current demand facts.
    pub demand: &'facts [Demand],
    /// Verified optional bundles and their observed residence.
    pub bundles: &'facts [BundleFact],
    /// Remote health from the adapter.
    pub remote_health: RemoteHealth,
    /// Explicit physical and retry budgets.
    pub budget: ResourceBudget,
}

/// Input collection whose fixed bound was exceeded.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, strum::AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum InputClass {
    /// [`PolicyInput::local`].
    Local,
    /// [`PolicyInput::remote`].
    Remote,
    /// [`PolicyInput::demand`].
    Demand,
    /// [`PolicyInput::bundles`].
    Bundle,
}

/// Exact duplicate authority rejected before decision selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DuplicateInput {
    /// Two local records claimed the same physical owner.
    Local {
        /// Duplicated canonical identity.
        key: FactKey,
        /// Duplicated physical owner.
        tier: StorageTier,
    },
    /// Two remote records claimed the same immutable fact.
    Remote {
        /// Duplicated canonical identity.
        key: FactKey,
    },
    /// Two demand records claimed the same immutable fact.
    Demand {
        /// Duplicated canonical identity.
        key: FactKey,
    },
    /// Two bundle records claimed the same capability artifact.
    Bundle {
        /// Duplicated optional capability.
        capability: CapabilityKind,
        /// Duplicated hash-pinned verified artifact.
        bundle: ContentId<CapabilityDomain>,
    },
}

/// Exact reason an immutable snapshot is not safe to plan from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyError {
    /// One caller-owned collection exceeded its declared bound.
    TooManyFacts {
        /// Rejected collection.
        class: InputClass,
        /// Fixed policy bound.
        limit: usize,
        /// Caller-provided cardinality.
        observed: usize,
    },
    /// A fact was supplied for another generation or snapshot.
    PinMismatch {
        /// Collection that carried the mismatched fact.
        class: InputClass,
        /// Authority selected by this input snapshot.
        expected: Pin,
        /// Fact retaining the mismatched authority.
        observed: FactKey,
    },
    /// Duplicate authority makes the input ambiguous.
    Duplicate(DuplicateInput),
}

/// Physical budget whose exact admission failed.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, strum::AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum ResourceClass {
    /// Process-addressable bytes.
    Ram,
    /// Local durable bytes.
    Nvme,
    /// Independent action slots.
    Operations,
    /// Remote recovery attempts.
    Retries,
}

/// Subject left untouched by an overloaded decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverloadSubject {
    /// A local physical copy.
    Fact(FactKey),
    /// A verified optional capability.
    Bundle {
        /// Optional capability kind.
        capability: CapabilityKind,
        /// Hash-pinned verified artifact.
        bundle: ContentId<CapabilityDomain>,
    },
    /// The pinned remote recovery contract.
    Remote(Pin),
}

/// Typed physical amount used to explain an overload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BudgetAmount {
    /// Byte reservation.
    Bytes(ByteCount),
    /// Action slot reservation.
    Operations(OperationBudget),
    /// Recovery attempt reservation.
    Retries(RetryBudget),
}

/// Exact resource admission failure; execution is not started.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Overload {
    /// Candidate that was not admitted.
    pub subject: OverloadSubject,
    /// Exhausted physical budget.
    pub resource: ResourceClass,
    /// Required amount.
    pub needed: BudgetAmount,
    /// Unchanged available amount.
    pub available: BudgetAmount,
}

/// Concrete remote inconsistency or outage preserved by recovery work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryCause {
    /// The remote adapter could not reach the service.
    Outage,
    /// The remote answered for different immutable authority.
    Inconsistent {
        /// Remote authority that conflicted with the consumer pin.
        observed: Pin,
    },
}

/// Exact action requested by the pure policy.
#[derive(Debug, Eq, PartialEq)]
pub enum PlacementAction {
    /// Move idle canonical bytes from RAM to local durable storage.
    PreserveToNvme {
        /// Canonical identity preserved byte-for-byte.
        key: FactKey,
        /// Exact transferred ownership.
        bytes: ByteCount,
    },
    /// Drop an idle RAM copy after proving the same pinned bytes remain on local durable storage.
    EvictFromRam {
        /// Canonical identity whose volatile duplicate is released.
        key: FactKey,
        /// Exact released bytes.
        bytes: ByteCount,
    },
    /// Activate a verified local bundle for recovery or local work.
    AcquireBundle {
        /// Typed capability that becomes active.
        capability: CapabilityKind,
        /// Hash-pinned verified artifact.
        bundle: ContentId<CapabilityDomain>,
    },
    /// Ask the remote adapter for one bounded recovery attempt.
    RetryRemote {
        /// Consumer-pinned authority that must be recovered.
        pin: Pin,
        /// Exact outage or inconsistency retained by the action.
        cause: RecoveryCause,
        /// Budget that remains after this one attempt.
        retries_remaining: RetryBudget,
    },
    /// Fetch a hot fact into RAM from a healthy matching remote.
    FetchToRam {
        /// Canonical identity preserved across the fetch.
        key: FactKey,
        /// Exact RAM reservation.
        bytes: ByteCount,
    },
    /// Drop an idle `NVMe` copy while retaining semantic identity elsewhere.
    EvictFromNvme {
        /// Canonical identity whose selected physical copy is released.
        key: FactKey,
        /// Exact released bytes.
        bytes: ByteCount,
    },
    /// Deactivate an optional capability bundle during contraction.
    ReleaseBundle {
        /// Typed capability to release.
        capability: CapabilityKind,
        /// Hash-pinned verified artifact.
        bundle: ContentId<CapabilityDomain>,
    },
}

/// Policy result. It is not an execution result.
#[derive(Debug, Eq, PartialEq)]
pub enum PolicyDecision {
    /// One canonical next action.
    Act(PlacementAction),
    /// No action is both safe and currently required.
    NoAction,
    /// Input authority is malformed or ambiguous.
    Rejected(PolicyError),
    /// A higher-priority action was deliberately not admitted.
    Overloaded(Overload),
    /// Recovery remains necessary but no retry credit is available.
    RecoveryExhausted {
        /// Pinned authority that remains unavailable.
        pin: Pin,
        /// Exact remote fault retained for later recovery.
        cause: RecoveryCause,
    },
}

/// One non-cloneable action owner handed from policy to an async adapter.
///
/// The adapter must keep this owner in exactly one [`ExecutionPoll`] phase until a terminal is
/// observed. The policy itself never creates a task, waker, queue, or mutable recovery ledger.
#[derive(Debug, Eq, PartialEq)]
pub struct ExecutionRequest {
    action: PlacementAction,
}

impl ExecutionRequest {
    /// Turns a policy action into the adapter-owned execution vocabulary.
    #[must_use]
    pub const fn new(action: PlacementAction) -> Self {
        Self { action }
    }

    /// Borrows the exact policy action without granting another owner.
    #[must_use]
    pub const fn action(&self) -> &PlacementAction {
        &self.action
    }

    /// Reports that the adapter armed its real wake source and remains pending.
    #[must_use]
    pub const fn pending(self) -> ExecutionPoll {
        ExecutionPoll::Pending(self)
    }

    /// Reports one completed item; the returned owner must still become terminal.
    #[must_use]
    pub const fn item(self) -> ExecutionPoll {
        ExecutionPoll::Item(ExecutionItem { request: self })
    }

    /// Ends the action after the adapter applied its one effect item.
    #[must_use]
    pub const fn completed(self) -> ExecutionTerminal {
        ExecutionTerminal::Completed { request: self }
    }

    /// Ends the action because cancellation won the adapter's linearization race.
    #[must_use]
    pub const fn cancelled(self) -> ExecutionTerminal {
        ExecutionTerminal::Cancelled { request: self }
    }

    /// Ends the action because cancellation won the adapter's linearization race.
    #[must_use]
    pub const fn cancel(self) -> ExecutionPoll {
        ExecutionPoll::Terminal(self.cancelled())
    }

    /// Ends the action with its exact external failure phase.
    #[must_use]
    pub const fn fail(self, phase: ExecutionPhase) -> ExecutionPoll {
        ExecutionPoll::Terminal(ExecutionTerminal::Failed {
            request: self,
            phase,
        })
    }
}

/// Runtime-independent async vocabulary: pending, one item, or one fused terminal.
#[derive(Debug, Eq, PartialEq)]
pub enum ExecutionPoll {
    /// The adapter registered its wake before returning this state.
    Pending(ExecutionRequest),
    /// One effect item completed and still owns its terminal transition.
    Item(ExecutionItem),
    /// The stream is terminal and must be fused by its adapter.
    Terminal(ExecutionTerminal),
}

/// A completed action whose terminal observation remains outstanding.
#[derive(Debug, Eq, PartialEq)]
pub struct ExecutionItem {
    request: ExecutionRequest,
}

impl ExecutionItem {
    /// Borrows the completed action for event publication without duplicating ownership.
    #[must_use]
    pub const fn action(&self) -> &PlacementAction {
        self.request.action()
    }

    /// Consumes the completed item and emits its one successful terminal.
    #[must_use]
    pub const fn finish(self) -> ExecutionPoll {
        ExecutionPoll::Terminal(ExecutionTerminal::Completed {
            request: self.request,
        })
    }
}

/// Fused typed execution terminal retaining the action owner and causal failure phase.
#[derive(Debug, Eq, PartialEq)]
pub enum ExecutionTerminal {
    /// The adapter applied the action and published a new immutable fact snapshot.
    Completed {
        /// The completed policy action owner.
        request: ExecutionRequest,
    },
    /// Cancellation won before an item was emitted.
    Cancelled {
        /// The cancelled policy action owner.
        request: ExecutionRequest,
    },
    /// The adapter failed while preserving the action and external phase.
    Failed {
        /// The uncompleted policy action.
        request: ExecutionRequest,
        /// Typed external phase that failed.
        phase: ExecutionPhase,
    },
}

impl ExecutionTerminal {
    /// Borrows the terminal's unique action owner.
    #[must_use]
    pub const fn action(&self) -> &PlacementAction {
        match self {
            Self::Completed { request }
            | Self::Cancelled { request }
            | Self::Failed { request, .. } => request.action(),
        }
    }
}

/// Concrete adapter phase that owns execution failure attribution.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, strum::AsRefStr)]
#[strum(serialize_all = "snake_case")]
pub enum ExecutionPhase {
    /// Local residence reservation or transfer.
    LocalResidence,
    /// Bundle activation or release.
    CapabilityBundle,
    /// Remote health or fetch operation.
    Remote,
}

