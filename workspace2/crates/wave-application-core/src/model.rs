//! Closed application vocabulary and bounded reply storage.

use crate::text::InputText;
use nudox_adaptive::{
    CapabilityDomain, CapabilityKind, ContentId, ExecutionPhase, Overload, Pin, PolicyError,
    RecoveryCause, ResourceBudget, RetryBudget,
};
use nudox_compile_vocab::FrontendError;

use crate::CompilerTerminal;

/// Largest result list accepted by the concrete service.
pub const MAX_REPLY_ROWS: u8 = 4;
/// Longest accepted semantic query or source input.
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

/// Closed typed input accepted by the in-process service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationInput {
    /// Compile one bounded source through an existing compiler-registry row.
    Generate {
        /// Request correlation.
        correlation: CorrelationId,
        /// Unvalidated compiler language token.
        language: InputText,
        /// Unvalidated compiler stage token.
        stage: InputText,
        /// Caller-bounded source bytes forwarded to the compiler registry.
        source: InputText,
    },
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
    pub const fn correlation(self) -> CorrelationId {
        match self {
            Self::Generate { correlation, .. }
            | Self::SnapshotStatus { correlation, .. }
            | Self::Search { correlation, .. }
            | Self::Graph { correlation, .. }
            | Self::Vector { correlation, .. }
            | Self::Locality { correlation, .. }
            | Self::Health { correlation }
            | Self::RecoverLocal { correlation, .. }
            | Self::ReleaseLocal { correlation, .. }
            | Self::PollExecution { correlation, .. }
            | Self::Cancel { correlation, .. } => correlation,
            Self::RecoverInconsistent(recovery) => recovery.correlation,
        }
    }
}

/// The one explicit request disposition returned by every semantic operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Terminal {
    /// A separate bounded operation owns the selected action.
    Accepted {
        /// Exact admitted operation.
        operation: OperationKey,
    },
    /// Full result coverage.
    Complete {
        /// Number of emitted rows.
        emitted: u8,
    },
    /// Partial result coverage with exact unavailable capability.
    Partial {
        /// Number of emitted rows.
        emitted: u8,
        /// Exact unavailable source.
        unavailable: Capability,
    },
    /// Useful local facts remain while a remote capability is unavailable.
    Degraded {
        /// Number of emitted rows.
        emitted: u8,
        /// Exact unavailable source.
        unavailable: Capability,
    },
    /// The named cancel action won the only operation transition.
    Cancelled {
        /// Number of emitted rows before cancellation.
        emitted: u8,
    },
    /// Semantic validation, policy validation, or a compiler row failed.
    Failed,
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
    /// The caller-owned policy snapshot was malformed or ambiguous.
    Rejected(PolicyError),
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

/// Structured business diagnostic code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    /// Unknown compiler language token.
    UnknownLanguage,
    /// Unknown compiler stage token.
    UnknownStage,
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplyBody {
    /// A real local compiler lowered and durably published one compact IR artifact.
    Generated(crate::GeneratedArtifact),
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
    /// One real poll or fused terminal from the service-owned execution future.
    Execution(ExecutionState),
    /// A typed failure has no fabricated payload.
    Rejected,
}

/// Fully structured service result shared by direct, CLI, MCP, and GPUI consumers.
#[derive(Debug, Eq, PartialEq)]
pub struct ApplicationReply {
    /// Correlation copied unchanged from the input.
    pub correlation: CorrelationId,
    /// One semantic body.
    pub body: ReplyBody,
    /// One request disposition.
    pub terminal: Terminal,
    /// A failure has exactly one source-preserving diagnostic in this slice.
    pub diagnostic: Option<Diagnostic>,
}

/// Coarse typed observation that service adapters may record lazily.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplicationEvent {
    /// Correlated reply event.
    pub correlation: CorrelationId,
    /// Exact resulting request disposition.
    pub terminal: Terminal,
}
