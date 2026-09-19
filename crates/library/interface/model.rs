//! Defines model behavior for `backend-library`, whose purpose is to own the transport-independent application service and reply vocabulary.
//! This module owns the model invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Closed application vocabulary and bounded reply storage.

use crate::interface::{
    InputText, PackageCompileRequest, RetrievalCause, RetrievalRows, SnapshotFacts, SourceText,
    UnloadReceipt,
};
use backend_execution::adaptive::{
    CapabilityDomain, CapabilityKind, ContentId, ExecutionPhase, Overload, Pin, PolicyError,
    RecoveryCause, ResourceBudget, RetryBudget,
};
use backend_semantic::vocabulary::{FrontendError, LanguageProfile, Stage};

use crate::interface::CompilerTerminal;

/// Largest result list accepted by the concrete service.
pub const MAX_REPLY_ROWS: u8 = 4;
/// Longest accepted semantic query or identifier input.
pub const MAX_SEMANTIC_TEXT_BYTES: usize = 32;

/// Application-wide correlation carried unchanged into replies and observations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CorrelationId(pub u64);

/// One stable operation identity for bounded execution and cancellation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationKey(pub u64);

/// One remote inconsistency recovery command with both immutable authorities retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InconsistentRecovery {
    /// Request correlation.
    pub correlation: CorrelationId,
    /// Generation and snapshot authority pinned by the caller.
    pub expected: Pin,
    /// Different generation or snapshot actually observed from the remote.
    pub observed: Pin,
    /// Hash-pinned verified analyzer bundle already resident locally.
    pub bundle: ContentId<CapabilityDomain>,
    /// Exact physical, action, and retry credits for this policy snapshot.
    pub budget: ResourceBudget,
}

/// Closed compiler target facts shared by direct, CLI, MCP, and file-backed source admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerateTarget {
    /// Request correlation.
    pub correlation: CorrelationId,
    /// Canonical compiler profile selected at the adapter boundary.
    pub profile: LanguageProfile,
    /// Canonical compiler stage selected at the adapter boundary.
    pub stage: Stage,
}

/// One fully admitted compiler request.
#[derive(Debug, Eq, PartialEq)]
pub struct GenerateRequest {
    /// Typed compiler target.
    pub target: GenerateTarget,
    /// Exact bounded source owner borrowed by the compiler during execution.
    pub source: SourceText,
}

/// Closed typed input accepted by the in-process service.
#[derive(Debug, Eq, PartialEq)]
pub enum ApplicationInput {
    /// Compile one bounded source through an existing backend-semantic registry row.
    Generate(GenerateRequest),
    /// Resolve, compile, publish, and reopen one pinned local package.
    CompilePackage(PackageCompileRequest),
    /// Inspect one immutable snapshot.
    SnapshotStatus {
        /// Request correlation.
        correlation: CorrelationId,
        /// Unvalidated snapshot selector.
        snapshot: InputText,
    },
    /// Search locally available snapshot facts.
    Search {
        /// Request correlation.
        correlation: CorrelationId,
        /// Unvalidated snapshot selector.
        snapshot: InputText,
        /// Unvalidated lexical query.
        query: InputText,
        /// Requested result count.
        limit: u8,
    },
    /// Request graph results from an absent upstream graph seam.
    Graph {
        /// Request correlation.
        correlation: CorrelationId,
        /// Unvalidated snapshot selector.
        snapshot: InputText,
        /// Requested result count.
        limit: u8,
    },
    /// Request vector results from an absent upstream vector seam.
    Vector {
        /// Request correlation.
        correlation: CorrelationId,
        /// Unvalidated snapshot selector.
        snapshot: InputText,
        /// Requested result count.
        limit: u8,
    },
    /// Inspect local snapshot residency.
    Locality {
        /// Request correlation.
        correlation: CorrelationId,
        /// Unvalidated snapshot selector.
        snapshot: InputText,
    },
    /// Unload one snapshot through the journaled, idempotent retrieval seam.
    RemoveIndex {
        /// Request correlation.
        correlation: CorrelationId,
        /// Unvalidated snapshot selector.
        snapshot: InputText,
    },
    /// Inspect local and unavailable capability health without inventing backend success.
    Health {
        /// Request correlation.
        correlation: CorrelationId,
    },
    /// Select the canonical local-first outage action for one immutable analyzer bundle.
    RecoverLocal {
        /// Request correlation.
        correlation: CorrelationId,
        /// Generation and snapshot authority pinned by the caller.
        pin: Pin,
        /// Hash-pinned verified analyzer bundle already resident locally.
        bundle: ContentId<CapabilityDomain>,
        /// Exact physical, action, and retry credits for this policy snapshot.
        budget: ResourceBudget,
    },
    /// Recover locally after a remote answered under a different immutable authority.
    RecoverInconsistent(InconsistentRecovery),
    /// Select a safe contraction action for the service-owned active analyzer bundle.
    ReleaseLocal {
        /// Request correlation.
        correlation: CorrelationId,
        /// Generation and snapshot authority pinned by the caller.
        pin: Pin,
        /// Hash-pinned verified analyzer bundle selected for release.
        bundle: ContentId<CapabilityDomain>,
        /// Exact physical, action, and retry credits for this policy snapshot.
        budget: ResourceBudget,
    },
    /// Poll exactly one service-owned adaptive execution future.
    PollExecution {
        /// Request correlation.
        correlation: CorrelationId,
        /// Operation whose wake/terminal state is requested.
        operation: OperationKey,
    },
    /// Cancel exactly one service-owned adaptive execution future.
    Cancel {
        /// Request correlation.
        correlation: CorrelationId,
        /// Operation to cancel.
        operation: OperationKey,
    },
}

impl ApplicationInput {
    /// Returns the unchanged caller correlation.
    #[must_use]
    pub const fn correlation(&self) -> CorrelationId {
        match self {
            Self::Generate(request) => request.target.correlation,
            Self::CompilePackage(request) => request.correlation(),
            Self::SnapshotStatus { correlation, .. }
            | Self::Search { correlation, .. }
            | Self::Graph { correlation, .. }
            | Self::Vector { correlation, .. }
            | Self::Locality { correlation, .. }
            | Self::RemoveIndex { correlation, .. }
            | Self::Health { correlation }
            | Self::RecoverLocal { correlation, .. }
            | Self::ReleaseLocal { correlation, .. }
            | Self::PollExecution { correlation, .. }
            | Self::Cancel { correlation, .. } => *correlation,
            Self::RecoverInconsistent(recovery) => recovery.correlation,
        }
    }
}

/// Named capability health, never an unstructured availability string.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Capability {
    /// The local compiler vocabulary registry.
    CompilerRegistry,
    /// A genuine compiler artifact, which the current registry does not produce.
    CompilerOutput,
    /// The immutable-index seam, unavailable until a real provider is integrated.
    Index,
    /// The absent remote graph provider.
    Graph,
    /// The absent remote vector provider.
    Vector,
    /// The optional local analyzer bundle governed by the adaptive policy.
    LocalAnalyzer,
    /// The remote recovery adapter.
    Remote,
}

/// One concrete health fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityHealth {
    /// A local capability is available now.
    LocalReady(Capability),
    /// A capability has no accepted provider or active residence yet.
    Unavailable(Capability),
}

/// One observable local capability transition selected by C6.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityTransition {
    /// Activate the verified analyzer bundle.
    Acquire {
        /// Typed adaptive capability.
        capability: CapabilityKind,
        /// Hash-pinned verified bundle.
        bundle: ContentId<CapabilityDomain>,
    },
    /// Release the verified analyzer bundle after the policy proves it idle.
    Release {
        /// Typed adaptive capability.
        capability: CapabilityKind,
        /// Hash-pinned verified bundle.
        bundle: ContentId<CapabilityDomain>,
    },
}

/// A pure adaptive decision that does not claim an executed effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdaptiveDisposition {
    /// No action was both safe and required.
    NoAction,
    /// One bounded remote retry is selected but has no provider in this slice.
    RetryRemote {
        /// Immutable authority retained across recovery.
        pin: Pin,
        /// Exact causal remote failure.
        cause: RecoveryCause,
        /// Credit remaining after the selected attempt.
        retries_remaining: RetryBudget,
    },
    /// Recovery remains necessary with no retry credit.
    RecoveryExhausted {
        /// Immutable authority still unavailable.
        pin: Pin,
        /// Exact causal remote failure.
        cause: RecoveryCause,
    },
    /// Resource admission rejected the otherwise canonical action.
    Overloaded(Overload),
}

/// One observation of the finite service-owned capability future.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionState {
    /// A real waker was registered and the action remains uniquely owned by the future.
    Pending {
        /// Polled operation.
        operation: OperationKey,
        /// Exact still-owned action.
        transition: CapabilityTransition,
    },
    /// The local metadata transition was applied and its owner reached a fused terminal.
    Completed {
        /// Completed operation.
        operation: OperationKey,
        /// Exact applied action.
        transition: CapabilityTransition,
    },
    /// Cancellation won before the action item was emitted.
    Cancelled {
        /// Cancelled operation.
        operation: OperationKey,
        /// Exact unexecuted action.
        transition: CapabilityTransition,
    },
    /// The external adapter failed in one typed phase without losing the action owner.
    Failed {
        /// Failed operation.
        operation: OperationKey,
        /// Exact unexecuted action.
        transition: CapabilityTransition,
        /// External phase retaining failure attribution.
        phase: ExecutionPhase,
    },
}

/// One non-failure execution observation that may be delivered as a reply body.
///
/// An execution failure always travels in [`DiagnosticDetail::Execution`], so a visible body
/// cannot manufacture a body-bearing failure without its typed cause.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionReply {
    /// A real waker was registered and the action remains uniquely owned by the future.
    Pending {
        /// Polled operation.
        operation: OperationKey,
        /// Exact still-owned action.
        transition: CapabilityTransition,
    },
    /// The local metadata transition was applied and reached its fused terminal.
    Completed {
        /// Completed operation.
        operation: OperationKey,
        /// Exact applied action.
        transition: CapabilityTransition,
    },
    /// Cancellation won before the action item was emitted.
    Cancelled {
        /// Cancelled operation.
        operation: OperationKey,
        /// Exact unexecuted action.
        transition: CapabilityTransition,
    },
}

impl From<ExecutionReply> for ExecutionState {
    fn from(reply: ExecutionReply) -> Self {
        match reply {
            ExecutionReply::Pending {
                operation,
                transition,
            } => Self::Pending {
                operation,
                transition,
            },
            ExecutionReply::Completed {
                operation,
                transition,
            } => Self::Completed {
                operation,
                transition,
            },
            ExecutionReply::Cancelled {
                operation,
                transition,
            } => Self::Cancelled {
                operation,
                transition,
            },
        }
    }
}

/// Structured business diagnostic code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    /// Semantic text exceeded the command limit.
    SemanticTextTooLong,
    /// Requested row count exceeded the fixed bound.
    ResultLimitExceeded,
    /// A lower-plane dependency is unavailable rather than counterfeited by the service.
    DependencyUnavailable,
    /// Existing compiler registry rejected the selected vocabulary row.
    UnsupportedCompilerStage,
    /// Cancellation or polling named an unknown operation.
    OperationUnavailable,
    /// Adaptive policy input was rejected with its typed cause retained.
    AdaptivePolicyRejected,
    /// A configured compiler or durable publication adapter returned one bounded typed terminal.
    CompilerTerminal,
    /// A local capability execution failed at one exact external phase.
    ExecutionFailed,
    /// The configured retrieval capability returned one exact typed cause.
    RetrievalFailed,
}

/// Exact rejected operand/cause retained by a business diagnostic.
#[derive(Debug, Eq, PartialEq)]
pub enum DiagnosticDetail {
    /// A validated transport token that failed closed semantic validation.
    Text(InputText),
    /// Exact requested and accepted result bounds.
    Limit {
        /// Rejected requested rows.
        requested: u8,
        /// Fixed service limit.
        maximum: u8,
    },
    /// Exact text length and semantic maximum.
    TextLength {
        /// Observed bytes.
        actual: usize,
        /// Accepted semantic bytes.
        maximum: usize,
        /// Complete field retained while it fits the transport bound.
        rejected: InputText,
    },
    /// Existing typed compiler rejection.
    Frontend(FrontendError),
    /// Exact unavailable operation identity.
    Operation(OperationKey),
    /// Exact lower-plane dependency that the service refuses to counterfeit.
    Capability(Capability),
    /// Exact adaptive validation cause.
    Policy(PolicyError),
    /// Closed compiler or publication terminal from the configured local capability.
    Compiler(CompilerTerminal),
    /// Exact local capability execution that failed before its intended transition applied.
    Execution(ExecutionState),
    /// Exact retrieval capability cause retained without a string projection.
    Retrieval(RetrievalCause),
}

/// One source-preserving service diagnostic.
#[derive(Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// Closed diagnostic code.
    pub code: DiagnosticCode,
    /// Exact rejected operand or existing source cause.
    pub detail: DiagnosticDetail,
}

/// Semantic reply body; all business behavior is represented here rather than in adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplyBody {
    /// A real local compiler lowered and durably published one compact IR artifact.
    Generated(crate::interface::GeneratedArtifact),
    /// A lower-plane dependency is not present in this application slice.
    DependencyUnavailable {
        /// Exact missing lower-plane capability.
        capability: Capability,
    },
    /// All known local/unavailable capability facts.
    Health([CapabilityHealth; 6]),
    /// A pure C6 decision whose external effect is not counterfeited.
    Adaptive(AdaptiveDisposition),
    /// C6 selected a local effect and a finite execution future owns it.
    ExecutionStarted {
        /// Stable handle for polling and cancellation.
        operation: OperationKey,
        /// Exact selected local effect.
        transition: CapabilityTransition,
    },
    /// One non-failure poll or fused terminal from the service-owned execution future.
    Execution(ExecutionReply),
    /// Honest residency facts for one snapshot selector.
    Snapshot(SnapshotFacts),
    /// Fixed-capacity retrieval rows in capability rank order.
    Retrieval(RetrievalRows),
    /// Journaled idempotent receipt for a requested index unload.
    IndexRemoved(UnloadReceipt),
}

/// Closed request result that makes success and diagnostic facts mutually exclusive.
#[derive(Debug, Eq, PartialEq)]
pub enum ApplicationOutcome {
    /// Visible semantic facts whose disposition is derived exclusively from their variant.
    Resolved(ReplyBody),
    /// Semantic validation or a lower-plane failure retained its one exact diagnostic.
    Failed {
        /// The sole source-preserving failure fact. Failure deliberately has no reply body.
        diagnostic: Diagnostic,
    },
}

/// Fully structured service result shared by direct, CLI, MCP, and GPUI consumers.
#[derive(Debug, Eq, PartialEq)]
pub struct ApplicationReply {
    /// Correlation copied unchanged from the input.
    pub correlation: CorrelationId,
    /// Closed result: a body with its disposition or a sole failure diagnostic.
    pub outcome: ApplicationOutcome,
}

/// Typed disposition of a body-bearing result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationDisposition {
    /// A bounded operation was admitted.
    Accepted {
        /// Stable handle assigned to the admitted operation.
        operation: OperationKey,
    },
    /// Full result coverage.
    Complete {
        /// Number of result rows emitted to the caller.
        emitted: u8,
    },
    /// Partial result coverage with one unavailable capability.
    Partial {
        /// Number of result rows emitted before the unavailable capability was encountered.
        emitted: u8,
        /// Capability whose absence prevented complete coverage.
        unavailable: Capability,
    },
    /// Degraded result coverage with one unavailable capability.
    Degraded {
        /// Number of useful result rows emitted by the degraded path.
        emitted: u8,
        /// Capability whose absence forced degraded execution.
        unavailable: Capability,
    },
    /// Cancellation won.
    Cancelled {
        /// Number of result rows emitted before cancellation became terminal.
        emitted: u8,
    },
}

/// Compact observation of a body-bearing disposition or a retained failure class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationObservation {
    /// One body-bearing outcome resolved with this disposition.
    Resolved(ApplicationDisposition),
    /// One diagnostic-bearing outcome failed with this closed code.
    Failed {
        /// Closed diagnostic class retained by the failed outcome.
        code: DiagnosticCode,
    },
    /// One ordered package-compilation phase was entered.
    PackagePhase {
        /// Exact phase; completion remains represented only by a resolved reply.
        phase: crate::interface::PackageCompilePhase,
    },
}

impl From<&ReplyBody> for ApplicationDisposition {
    fn from(body: &ReplyBody) -> Self {
        match body {
            ReplyBody::Generated(_) | ReplyBody::Execution(ExecutionReply::Completed { .. }) => {
                Self::Complete { emitted: 1 }
            }
            ReplyBody::DependencyUnavailable { capability } => Self::Degraded {
                emitted: 0,
                unavailable: *capability,
            },
            ReplyBody::Health(facts) => health_disposition(*facts),
            ReplyBody::Adaptive(AdaptiveDisposition::NoAction) => Self::Complete { emitted: 0 },
            ReplyBody::Adaptive(
                AdaptiveDisposition::RetryRemote { .. }
                | AdaptiveDisposition::RecoveryExhausted { .. },
            ) => Self::Degraded {
                emitted: 0,
                unavailable: Capability::Remote,
            },
            ReplyBody::Adaptive(AdaptiveDisposition::Overloaded(_)) => Self::Degraded {
                emitted: 0,
                unavailable: Capability::LocalAnalyzer,
            },
            ReplyBody::ExecutionStarted { operation, .. }
            | ReplyBody::Execution(ExecutionReply::Pending { operation, .. }) => Self::Accepted {
                operation: *operation,
            },
            ReplyBody::Execution(ExecutionReply::Cancelled { .. }) => {
                Self::Cancelled { emitted: 0 }
            }
            ReplyBody::Snapshot(_) | ReplyBody::IndexRemoved(_) => Self::Complete { emitted: 1 },
            ReplyBody::Retrieval(rows) => Self::Complete {
                emitted: rows.len(),
            },
        }
    }
}

/// Coarse typed observation that service adapters may record lazily.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationEvent {
    /// Correlated reply event.
    pub correlation: CorrelationId,
    /// Exact resulting class without duplicating body or diagnostic storage.
    pub outcome: ApplicationObservation,
}

impl From<&ApplicationReply> for ApplicationEvent {
    fn from(reply: &ApplicationReply) -> Self {
        let outcome = match &reply.outcome {
            ApplicationOutcome::Resolved(body) => {
                ApplicationObservation::Resolved(ApplicationDisposition::from(body))
            }
            ApplicationOutcome::Failed { diagnostic } => ApplicationObservation::Failed {
                code: diagnostic.code,
            },
        };
        Self {
            correlation: reply.correlation,
            outcome,
        }
    }
}

fn health_disposition(facts: [CapabilityHealth; 6]) -> ApplicationDisposition {
    let mut emitted = 0_u8;
    let mut first_unavailable = None;
    for fact in facts {
        match fact {
            CapabilityHealth::LocalReady(_) => emitted = emitted.saturating_add(1),
            CapabilityHealth::Unavailable(capability) if first_unavailable.is_none() => {
                first_unavailable = Some(capability);
            }
            CapabilityHealth::Unavailable(_) => {}
        }
    }
    match first_unavailable {
        Some(unavailable) => ApplicationDisposition::Partial {
            emitted,
            unavailable,
        },
        None => ApplicationDisposition::Complete { emitted },
    }
}
