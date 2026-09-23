//! Bounded, typed control-plane failures.

use core::fmt;

/// Errors raised while validating or advancing the agent control plane.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ControlError {
    /// A human-readable facet label was empty or exceeded the input bound.
    #[error("control-plane label is empty or oversized")]
    InvalidLabel,
    /// A digest did not contain exactly 32 bytes in hexadecimal form.
    #[error("control-plane digest has the wrong length")]
    InvalidDigestLength,
    /// A digest contains a non-hexadecimal byte.
    #[error("control-plane digest is not hexadecimal")]
    InvalidDigest,
    /// A supplied value exceeded one of the canonical wire bounds.
    #[error("control-plane value exceeds its bound")]
    Bounds,
    /// A work key's dependency list was not strictly ordered and unique.
    #[error("dependency keys must be strictly ordered")]
    NonCanonicalDependencies,
    /// A context delta repeated one audience/evidence entry.
    #[error("context entries must be strictly ordered and unique")]
    NonCanonicalContext,
    /// A work key was already used for a different immutable specification.
    #[error("work key collision with a different immutable specification")]
    WorkKeyCollision,
    /// A work item was not present in the versioned work relation.
    #[error("work item is unknown")]
    UnknownWork,
    /// A required prerequisite was absent or not terminal-successful.
    #[error("work item is waiting for a dependency")]
    DependencyNotReady,
    /// The owner supplied an attempt fence that is no longer current.
    #[error("attempt fence is stale or belongs to another owner")]
    StaleFence,
    /// The attempt's lease has passed its bounded expiry.
    #[error("attempt lease has expired")]
    LeaseExpired,
    /// The requested operation is not valid for the current state.
    #[error("invalid lifecycle transition")]
    InvalidTransition,
    /// The bounded waiter set for an active attempt is full.
    #[error("work item waiter limit reached")]
    WaiterLimit,
    /// The bounded retry budget has been consumed.
    #[error("work item retry budget exhausted")]
    RetryLimit,
    /// The scheduler's bounded active-attempt budget is full.
    #[error("scheduler parallelism limit reached")]
    ParallelismLimit,
    /// A completion was authored by the wrong authority.
    #[error("completion authority does not own the attempt")]
    CompletionAuthority,
    /// A custody authority duplicated an earlier actor in the receipt chain.
    #[error("evaluation, review, and decision authorities must be independent")]
    AuthorityNotIndependent,
    /// A typed evidence receipt cited missing, duplicate, or mismatched
    /// references.
    #[error("evidence receipt references are missing or inconsistent")]
    EvidenceBinding,
    /// A typed evidence receipt was authored by an authority that does not
    /// own that receipt kind.
    #[error("evidence receipt authority does not own this receipt kind")]
    EvidenceAuthority,
    /// A mutation was based on an earlier relation root.
    #[error("versioned control relation base root is stale")]
    StaleRoot,
    /// A persisted object or relation could not be admitted.
    #[error("durable control-plane state is corrupt")]
    Corrupt,
    /// The underlying store failed without exposing an unsafe partial state.
    #[error("durable control-plane store failed: {0}")]
    Store(String),
    /// A JSON/Nu adapter did not satisfy the versioned wire grammar.
    #[error("invalid control-plane wire request: {0}")]
    Wire(String),
    /// A command-line invocation omitted a required option.
    #[error("missing command-line argument: {0}")]
    MissingArgument(&'static str),
    /// The command-line operation was not recognized.
    #[error("unknown control-plane command")]
    UnknownCommand,
}

impl From<backend_store::StoreError> for ControlError {
    fn from(error: backend_store::StoreError) -> Self {
        Self::Store(format!("{error:?}"))
    }
}

impl From<backend_version::DeltaError> for ControlError {
    fn from(error: backend_version::DeltaError) -> Self {
        match error {
            backend_version::DeltaError::BaseMismatch
            | backend_version::DeltaError::BeforeMismatch => Self::StaleRoot,
            backend_version::DeltaError::IncompleteBase
            | backend_version::DeltaError::Canonical(_)
            | backend_version::DeltaError::DuplicateKey
            | backend_version::DeltaError::TargetMismatch
            | backend_version::DeltaError::Unsorted
            | backend_version::DeltaError::NonAdjacent
            | backend_version::DeltaError::CompositionMismatch => Self::Corrupt,
        }
    }
}

impl From<backend_version::WorkspaceError> for ControlError {
    fn from(error: backend_version::WorkspaceError) -> Self {
        Self::Store(error.to_string())
    }
}

impl fmt::Display for crate::record::AttemptOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::Quarantined => "quarantined",
        })
    }
}
