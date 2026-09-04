//! Defines json wire application behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire application invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use compiler_vocabulary::{Language, Stage};
use heart_adaptive::CapabilityDomain;
use interface_core::{
    CapabilityHealth, CapabilityTransition, Diagnostic, DiagnosticCode, DiagnosticDetail,
    DocSection, ExecutionReply, ExecutionState, GeneratedArtifact, ReplyBody, RetrievalCause,
    RetrievalMode, RetrievalPhase, RetrievalQueryCause, RetrievalRow, SignatureToken, TokenKind,
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
    Snapshot {
        snapshot: Text,
        resident: bool,
    },
    Retrieval {
        rows: Vec<RetrievalRowWire>,
    },
    IndexRemoved {
        snapshot: Text,
        removed: bool,
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
    RetrievalFailed,
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
    Retrieval {
        cause: RetrievalCauseWire,
    },
}

#[derive(Serialize)]
pub(super) struct RetrievalRowWire {
    document: Text,
    term: Text,
    start: Option<u32>,
    end: Option<u32>,
    score: u32,
    mode: RetrievalModeWire,
    signature: Option<Vec<SignatureTokenWire>>,
    section: DocSectionWire,
}

#[derive(Serialize)]
struct SignatureTokenWire {
    kind: TokenKindWire,
    text: Text,
    target: Option<Text>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum TokenKindWire {
    Keyword,
    Name,
    Type,
    Punctuation,
    Text,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum RetrievalModeWire {
    Exact,
    Lexical,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum DocSectionWire {
    Summary,
    Members,
    Fields,
    Source,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RetrievalCauseWire {
    SnapshotUnknown { snapshot: Text },
    QueryRejected { reason: RetrievalQueryCauseWire },
    Backend { phase: RetrievalPhaseWire },
    RowTableFull { rejected: RetrievalRowWire },
    JournalFull { rejected: Text },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RetrievalQueryCauseWire {
    Empty,
    Unsupported { observed: Text },
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum RetrievalPhaseWire {
    Scan,
    Rank,
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
            ReplyBody::Snapshot(facts) => Self::Snapshot {
                snapshot: Text(facts.snapshot),
                resident: facts.resident,
            },
            ReplyBody::Retrieval(rows) => Self::Retrieval {
                rows: rows.iter().copied().map(Into::into).collect(),
            },
            ReplyBody::IndexRemoved(receipt) => Self::IndexRemoved {
                snapshot: Text(receipt.snapshot),
                removed: receipt.removed,
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
            DiagnosticCode::RetrievalFailed => Self::RetrievalFailed,
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
            DiagnosticDetail::Retrieval(cause) => Self::Retrieval {
                cause: cause.into(),
            },
        }
    }
}

impl From<RetrievalRow> for RetrievalRowWire {
    fn from(row: RetrievalRow) -> Self {
        Self {
            document: Text(row.document),
            term: Text(row.term),
            start: row.span.map(|span| span.start),
            end: row.span.map(|span| span.end),
            score: row.score,
            mode: row.mode.into(),
            signature: row
                .signature
                .map(|tokens| tokens.iter().copied().map(Into::into).collect()),
            section: row.section.into(),
        }
    }
}

impl From<SignatureToken> for SignatureTokenWire {
    fn from(token: SignatureToken) -> Self {
        Self {
            kind: token.kind.into(),
            text: Text(token.text),
            target: token.target.map(Text),
        }
    }
}

impl From<TokenKind> for TokenKindWire {
    fn from(kind: TokenKind) -> Self {
        match kind {
            TokenKind::Keyword => Self::Keyword,
            TokenKind::Name => Self::Name,
            TokenKind::Type => Self::Type,
            TokenKind::Punctuation => Self::Punctuation,
            TokenKind::Text => Self::Text,
        }
    }
}

impl From<RetrievalMode> for RetrievalModeWire {
    fn from(mode: RetrievalMode) -> Self {
        match mode {
            RetrievalMode::Exact => Self::Exact,
            RetrievalMode::Lexical => Self::Lexical,
        }
    }
}

impl From<DocSection> for DocSectionWire {
    fn from(section: DocSection) -> Self {
        match section {
            DocSection::Summary => Self::Summary,
            DocSection::Members => Self::Members,
            DocSection::Fields => Self::Fields,
            DocSection::Source => Self::Source,
        }
    }
}

impl From<RetrievalCause> for RetrievalCauseWire {
    fn from(cause: RetrievalCause) -> Self {
        match cause {
            RetrievalCause::SnapshotUnknown { snapshot } => Self::SnapshotUnknown {
                snapshot: Text(snapshot),
            },
            RetrievalCause::QueryRejected { reason } => Self::QueryRejected {
                reason: reason.into(),
            },
            RetrievalCause::Backend { phase } => Self::Backend {
                phase: phase.into(),
            },
            RetrievalCause::RowTableFull { rejected } => Self::RowTableFull {
                rejected: rejected.into(),
            },
            RetrievalCause::JournalFull { rejected } => Self::JournalFull {
                rejected: Text(rejected),
            },
        }
    }
}

impl From<RetrievalQueryCause> for RetrievalQueryCauseWire {
    fn from(reason: RetrievalQueryCause) -> Self {
        match reason {
            RetrievalQueryCause::Empty => Self::Empty,
            RetrievalQueryCause::Unsupported { observed } => Self::Unsupported {
                observed: Text(observed),
            },
        }
    }
}

impl From<RetrievalPhase> for RetrievalPhaseWire {
    fn from(phase: RetrievalPhase) -> Self {
        match phase {
            RetrievalPhase::Scan => Self::Scan,
            RetrievalPhase::Rank => Self::Rank,
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
