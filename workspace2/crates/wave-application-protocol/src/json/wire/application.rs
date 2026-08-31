use nudox_adaptive::CapabilityDomain;
use serde::Serialize;
use wave_application_core::{
    CapabilityHealth, CapabilityTransition, Diagnostic, DiagnosticCode, DiagnosticDetail,
    ExecutionState, ReplyBody, Terminal,
};

use super::{
    adaptive::{AdaptiveDisposition, PolicyErrorWire},
    scalar::{CapabilityKind, CapabilityName, ContentText, ExecutionPhase, Language, Stage, Text},
};

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ReplyBodyWire {
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
        state: ExecutionStateWire,
    },
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
    UnknownLanguage,
    UnknownStage,
    SemanticTextTooLong,
    ResultLimitExceeded,
    DependencyUnavailable,
    UnsupportedCompilerStage,
    CompilerOutputUnrepresentable,
    OperationUnavailable,
    AdaptivePolicyRejected,
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
        language: Language,
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
            ReplyBody::Rejected => Self::Rejected,
        }
    }
}

impl From<Terminal> for TerminalWire {
    fn from(terminal: Terminal) -> Self {
        match terminal {
            Terminal::Accepted { operation } => Self::Accepted {
                operation: operation.0,
            },
            Terminal::Complete { emitted } => Self::Complete { emitted },
            Terminal::Partial {
                emitted,
                unavailable,
            } => Self::Partial {
                emitted,
                unavailable: unavailable.into(),
            },
            Terminal::Degraded {
                emitted,
                unavailable,
            } => Self::Degraded {
                emitted,
                unavailable: unavailable.into(),
            },
            Terminal::Cancelled { emitted } => Self::Cancelled { emitted },
            Terminal::Failed => Self::Failed,
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
            DiagnosticCode::UnknownLanguage => Self::UnknownLanguage,
            DiagnosticCode::UnknownStage => Self::UnknownStage,
            DiagnosticCode::SemanticTextTooLong => Self::SemanticTextTooLong,
            DiagnosticCode::ResultLimitExceeded => Self::ResultLimitExceeded,
            DiagnosticCode::DependencyUnavailable => Self::DependencyUnavailable,
            DiagnosticCode::UnsupportedCompilerStage => Self::UnsupportedCompilerStage,
            DiagnosticCode::CompilerOutputUnrepresentable => Self::CompilerOutputUnrepresentable,
            DiagnosticCode::OperationUnavailable => Self::OperationUnavailable,
            DiagnosticCode::AdaptivePolicyRejected => Self::AdaptivePolicyRejected,
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
                nudox_compile_vocab::FrontendError::UnsupportedStage { language, stage } => {
                    Self::UnsupportedStage {
                        language: language.into(),
                        stage: stage.into(),
                    }
                }
            },
            DiagnosticDetail::Operation(operation) => Self::Operation { value: operation.0 },
            DiagnosticDetail::Capability(capability) => Self::Capability {
                value: capability.into(),
            },
            DiagnosticDetail::Policy(error) => Self::Policy {
                error: error.into(),
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
