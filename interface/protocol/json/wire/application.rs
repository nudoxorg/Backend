//! Defines json wire application behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire application invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use compiler_vocabulary::{Language, Stage};
use heart_adaptive::CapabilityDomain;
use interface_core::{
    CapabilityHealth, CapabilityTransition, Diagnostic, DiagnosticCode, DiagnosticDetail,
    ExecutionReply, ExecutionState, GeneratedArtifact, ReplyBody,
};
use serde::Serialize;

use super::{
    adaptive::{AdaptiveDisposition, PolicyErrorWire},
    compiler::{CompilerTerminalWire, GeneratedArtifactWire},
    scalar::{CapabilityKind, CapabilityName, ContentText, ExecutionPhase, Text},
};

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ReplyBodyWire {
    Generated {
        #[serde(with = "GeneratedArtifactWire")]
        artifact: GeneratedArtifact,
    },
    DependencyUnavailable {
        capability: CapabilityName,
    },
    Health {
        facts: [CapabilityHealthWire; 6],
    },
    Adaptive {
        disposition: AdaptiveDisposition,
    },
    ExecutionStarted {
        operation: u64,
        transition: CapabilityTransitionWire,
    },
    Execution {
        state: ExecutionReplyWire,
    },
    /// Legacy body slot retained by the external envelope for a diagnostic-only outcome.
    Rejected,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum TerminalWire {
    Accepted {
        operation: u64,
    },
    Complete {
        emitted: u8,
    },
    Partial {
        emitted: u8,
        unavailable: CapabilityName,
    },
    Degraded {
        emitted: u8,
        unavailable: CapabilityName,
    },
    Cancelled {
        emitted: u8,
    },
    Failed,
}

#[derive(Serialize)]
pub(super) struct DiagnosticWire {
    code: DiagnosticCodeWire,
    detail: DiagnosticDetailWire,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum DiagnosticCodeWire {
    SemanticTextTooLong,
    ResultLimitExceeded,
    DependencyUnavailable,
    UnsupportedCompilerStage,
    OperationUnavailable,
    AdaptivePolicyRejected,
    CompilerTerminal,
    ExecutionFailed,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DiagnosticDetailWire {
    Text {
        value: Text,
    },
    Limit {
        requested: u8,
        maximum: u8,
    },
    TextLength {
        actual: usize,
        maximum: usize,
        rejected: Text,
    },
    UnsupportedStage {
        #[serde(with = "super::scalar::LanguageWire")]
        language: Language,
        #[serde(with = "super::scalar::StageWire")]
        stage: Stage,
    },
    Operation {
        value: u64,
    },
    Capability {
        value: CapabilityName,
    },
    Policy {
        error: PolicyErrorWire,
    },
    Compiler {
        #[serde(with = "CompilerTerminalWire")]
        terminal: interface_core::CompilerTerminal,
    },
    Execution {
        state: ExecutionStateWire,
    },
}

#[derive(Serialize)]
pub(super) struct CapabilityHealthWire {
    capability: CapabilityName,
    state: CapabilityStateWire,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CapabilityStateWire {
    LocalReady,
    Unavailable,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum CapabilityTransitionWire {
    Acquire {
        capability: CapabilityKind,
        bundle: ContentText<CapabilityDomain>,
    },
    Release {
        capability: CapabilityKind,
        bundle: ContentText<CapabilityDomain>,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ExecutionReplyWire {
    Pending {
        operation: u64,
        transition: CapabilityTransitionWire,
    },
    Completed {
        operation: u64,
        transition: CapabilityTransitionWire,
    },
    Cancelled {
        operation: u64,
        transition: CapabilityTransitionWire,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ExecutionStateWire {
    Pending {
        operation: u64,
        transition: CapabilityTransitionWire,
    },
    Completed {
        operation: u64,
        transition: CapabilityTransitionWire,
    },
    Cancelled {
        operation: u64,
        transition: CapabilityTransitionWire,
    },
    Failed {
        operation: u64,
        transition: CapabilityTransitionWire,
        phase: ExecutionPhase,
    },
}

impl From<ReplyBody> for ReplyBodyWire {
    fn from(body: ReplyBody) -> Self {
        match body {
            ReplyBody::Generated(artifact) => Self::Generated { artifact },
            ReplyBody::DependencyUnavailable { capability } => Self::DependencyUnavailable {
                capability: capability.into(),
            },
            ReplyBody::Health(facts) => Self::Health {
                facts: facts.map(Into::into),
            },
            ReplyBody::Adaptive(disposition) => Self::Adaptive {
                disposition: disposition.into(),
            },
            ReplyBody::ExecutionStarted {
                operation,
                transition,
            } => Self::ExecutionStarted {
                operation: operation.0,
                transition: transition.into(),
            },
            ReplyBody::Execution(state) => Self::Execution {
                state: state.into(),
            },
        }
    }
}

impl From<Diagnostic> for DiagnosticWire {
    fn from(diagnostic: Diagnostic) -> Self {
        Self {
            code: diagnostic.code.into(),
            detail: diagnostic.detail.into(),
        }
    }
}

impl From<DiagnosticCode> for DiagnosticCodeWire {
    fn from(code: DiagnosticCode) -> Self {
        match code {
            DiagnosticCode::SemanticTextTooLong => Self::SemanticTextTooLong,
            DiagnosticCode::ResultLimitExceeded => Self::ResultLimitExceeded,
            DiagnosticCode::DependencyUnavailable => Self::DependencyUnavailable,
            DiagnosticCode::UnsupportedCompilerStage => Self::UnsupportedCompilerStage,
            DiagnosticCode::OperationUnavailable => Self::OperationUnavailable,
            DiagnosticCode::AdaptivePolicyRejected => Self::AdaptivePolicyRejected,
            DiagnosticCode::CompilerTerminal => Self::CompilerTerminal,
            DiagnosticCode::ExecutionFailed => Self::ExecutionFailed,
        }
    }
}

impl From<DiagnosticDetail> for DiagnosticDetailWire {
    fn from(detail: DiagnosticDetail) -> Self {
        match detail {
            DiagnosticDetail::Text(value) => Self::Text { value: Text(value) },
            DiagnosticDetail::Limit { requested, maximum } => Self::Limit { requested, maximum },
            DiagnosticDetail::TextLength {
                actual,
                maximum,
                rejected,
            } => Self::TextLength {
                actual,
                maximum,
                rejected: Text(rejected),
            },
            DiagnosticDetail::Frontend(source) => match source {
                compiler_vocabulary::FrontendError::UnsupportedStage { language, stage } => {
                    Self::UnsupportedStage { language, stage }
                }
            },
            DiagnosticDetail::Operation(operation) => Self::Operation { value: operation.0 },
            DiagnosticDetail::Capability(capability) => Self::Capability {
                value: capability.into(),
            },
            DiagnosticDetail::Policy(error) => Self::Policy {
                error: error.into(),
            },
            DiagnosticDetail::Compiler(terminal) => Self::Compiler { terminal },
            DiagnosticDetail::Execution(state) => Self::Execution {
                state: state.into(),
            },
        }
    }
}

impl From<CapabilityHealth> for CapabilityHealthWire {
    fn from(fact: CapabilityHealth) -> Self {
        match fact {
            CapabilityHealth::LocalReady(capability) => Self {
                capability: capability.into(),
                state: CapabilityStateWire::LocalReady,
            },
            CapabilityHealth::Unavailable(capability) => Self {
                capability: capability.into(),
                state: CapabilityStateWire::Unavailable,
            },
        }
    }
}

impl From<CapabilityTransition> for CapabilityTransitionWire {
    fn from(transition: CapabilityTransition) -> Self {
        match transition {
            CapabilityTransition::Acquire { capability, bundle } => Self::Acquire {
                capability: capability.into(),
                bundle: ContentText(bundle),
            },
            CapabilityTransition::Release { capability, bundle } => Self::Release {
                capability: capability.into(),
                bundle: ContentText(bundle),
            },
        }
    }
}

impl From<ExecutionReply> for ExecutionReplyWire {
    fn from(state: ExecutionReply) -> Self {
        match state {
            ExecutionReply::Pending {
                operation,
                transition,
            } => Self::Pending {
                operation: operation.0,
                transition: transition.into(),
            },
            ExecutionReply::Completed {
                operation,
                transition,
            } => Self::Completed {
                operation: operation.0,
                transition: transition.into(),
            },
            ExecutionReply::Cancelled {
                operation,
                transition,
            } => Self::Cancelled {
                operation: operation.0,
                transition: transition.into(),
            },
        }
    }
}

impl From<ExecutionState> for ExecutionStateWire {
    fn from(state: ExecutionState) -> Self {
        match state {
            ExecutionState::Pending {
                operation,
                transition,
            } => Self::Pending {
                operation: operation.0,
                transition: transition.into(),
            },
            ExecutionState::Completed {
                operation,
                transition,
            } => Self::Completed {
                operation: operation.0,
                transition: transition.into(),
            },
            ExecutionState::Cancelled {
                operation,
                transition,
            } => Self::Cancelled {
                operation: operation.0,
                transition: transition.into(),
            },
            ExecutionState::Failed {
                operation,
                transition,
                phase,
            } => Self::Failed {
                operation: operation.0,
                transition: transition.into(),
                phase: phase.into(),
            },
        }
    }
}
