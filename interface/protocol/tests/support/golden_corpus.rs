//! Exercises the `interface-protocol` tests support golden-corpus contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Typed, deterministic records shared by the CLI and MCP process journeys.
//!
//! This is test support rather than a second application protocol.  Process records are
//! deserialized through the production reply boundary so an independently constructed direct
//! service reply, a CLI line, and an MCP `structuredContent` value all have to agree on the same
//! closed vocabulary.

use compiler_vocabulary::{LanguageProfile, RustEdition, Stage};
use interface_core::{
    ApplicationDisposition, ApplicationInput, ApplicationOutcome, ApplicationReply,
    ApplicationService, BatteryState, ByteCount, Capability, CapabilityDomain, CapabilityHealth,
    CapabilityKind, CapabilityTransition, ContentId, CorrelationId, Diagnostic, DiagnosticCode,
    DiagnosticDetail, ExecutionReply, ExecutionState, GenerateRequest, GenerateTarget,
    GeneratedArtifact, GenerationId, IndexSnapshotId, InputText, InputTextError, OperationBudget,
    OperationKey, Pin, Pressure, RejectedSourceText, ReplyBody, ResourceBudget, RetryBudget,
    SourceText,
};
use serde::Deserialize;

mod compiler;

#[allow(unused_imports)]
pub(crate) use compiler::{
    GoldenCompileRecipe, GoldenCompilerAttempt, GoldenCompilerCause, GoldenCompilerDiagnostic,
    GoldenCompilerTerminal, GoldenErrorKind, GoldenFragmentCause, GoldenGeneratedArtifact,
    GoldenGenerationAuthority, GoldenLanguage, GoldenLoweringCause, GoldenNativeArtifactAction,
    GoldenNativeArtifactRole, GoldenNativeDirectoryCause, GoldenNativeIoFact, GoldenNativeIoPhase,
    GoldenNativePrimaryCause, GoldenNativeTool, GoldenNativeWorkCause,
    GoldenNativeWorkCleanupCause, GoldenNativeWorkPhase, GoldenPublicationAuthority,
    GoldenPublicationCause, GoldenPublicationPhase, GoldenSourceAuthority, GoldenStage,
};

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenReply {
    pub correlation: u64,
    pub body: GoldenBody,
    pub terminal: GoldenTerminal,
    pub diagnostic: Option<GoldenDiagnostic>,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenBody {
    Generated {
        artifact: GoldenGeneratedArtifact,
    },
    DependencyUnavailable {
        capability: GoldenCapability,
    },
    Health {
        facts: [GoldenHealth; 6],
    },
    ExecutionStarted {
        operation: u64,
        transition: GoldenTransition,
    },
    Execution {
        state: GoldenExecutionState,
    },
    Rejected,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCapability {
    CompilerRegistry,
    CompilerOutput,
    Index,
    Graph,
    Vector,
    LocalAnalyzer,
    Remote,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenHealth {
    pub capability: GoldenCapability,
    pub state: GoldenHealthState,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenHealthState {
    LocalReady,
    Unavailable,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenTransition {
    Acquire {
        capability: GoldenCapabilityKind,
        bundle: String,
    },
    Release {
        capability: GoldenCapabilityKind,
        bundle: String,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCapabilityKind {
    Analyzer,
    Compiler,
    Codec,
    Model,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenExecutionState {
    Pending {
        operation: u64,
        transition: GoldenTransition,
    },
    Completed {
        operation: u64,
        transition: GoldenTransition,
    },
    Cancelled {
        operation: u64,
        transition: GoldenTransition,
    },
    Failed {
        operation: u64,
        transition: GoldenTransition,
        phase: GoldenExecutionPhase,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenExecutionPhase {
    LocalResidence,
    CapabilityBundle,
    Remote,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenTerminal {
    Accepted {
        operation: u64,
    },
    Complete {
        emitted: u8,
    },
    Partial {
        emitted: u8,
        unavailable: GoldenCapability,
    },
    Degraded {
        emitted: u8,
        unavailable: GoldenCapability,
    },
    Cancelled {
        emitted: u8,
    },
    Failed,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenDiagnostic {
    pub code: GoldenDiagnosticCode,
    pub detail: GoldenDiagnosticDetail,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenDiagnosticCode {
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

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenDiagnosticDetail {
    Capability { value: GoldenCapability },
    Compiler { terminal: GoldenCompilerTerminal },
    Execution { state: GoldenExecutionState },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenMcpResponse {
    pub id: GoldenResponseId,
    pub result: GoldenMcpResult,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
pub(crate) enum GoldenResponseId {
    Null,
    Number(u64),
    Text(String),
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenMcpResult {
    #[serde(rename = "structuredContent")]
    pub structured_content: GoldenReply,
}

pub(crate) struct GoldenInputs {
    pub generate: ApplicationInput,
    pub health: ApplicationInput,
    pub recover: ApplicationInput,
    pub cancel: ApplicationInput,
}

pub(crate) fn command_arguments() -> [Vec<String>; 4] {
    let generation = GenerationId::from_digest([11; 32]);
    let snapshot = IndexSnapshotId::from_digest([13; 32]);
    let bundle = ContentId::<CapabilityDomain>::from_digest([17; 32]);
    let generation_text = generation.to_string();
    let snapshot_text = snapshot.to_string();
    let bundle_text = bundle.to_string();
    [
        ["generate", "101", "rust-2024", "lower-ir", "fn corpus() {}"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        ["health", "102"].into_iter().map(str::to_owned).collect(),
        [
            "recover-local",
            "103",
            generation_text.as_str(),
            snapshot_text.as_str(),
            bundle_text.as_str(),
            "4096",
            "8192",
            "1",
            "1",
            "relaxed",
            "relaxed",
            "normal",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        ["cancel", "104", "1"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    ]
}

pub(crate) fn inputs() -> Result<GoldenInputs, GoldenError> {
    let generation = GenerationId::from_digest([11; 32]);
    let snapshot = IndexSnapshotId::from_digest([13; 32]);
    let bundle = ContentId::<CapabilityDomain>::from_digest([17; 32]);
    let pin = Pin {
        generation,
        snapshot,
    };
    Ok(GoldenInputs {
        generate: generate(
            101,
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            "fn corpus() {}",
        )?,
        health: ApplicationInput::Health {
            correlation: CorrelationId(102),
        },
        recover: ApplicationInput::RecoverLocal {
            correlation: CorrelationId(103),
            pin,
            bundle,
            budget: budget(),
        },
        cancel: ApplicationInput::Cancel {
            correlation: CorrelationId(104),
            operation: OperationKey(1),
        },
    })
}

pub(crate) fn direct_replies() -> Result<[GoldenReply; 4], GoldenError> {
    let [generate, health, recover, cancel] = direct_application_replies()?;
    Ok([
        GoldenReply::from_reply(generate)?,
        GoldenReply::from_reply(health)?,
        GoldenReply::from_reply(recover)?,
        GoldenReply::from_reply(cancel)?,
    ])
}

pub(crate) fn direct_application_replies() -> Result<[ApplicationReply; 4], GoldenError> {
    let inputs = inputs()?;
    let mut service = ApplicationService::new();
    Ok([
        service.execute(&inputs.generate),
        service.execute(&inputs.health),
        service.execute(&inputs.recover),
        service.execute(&inputs.cancel),
    ])
}

pub(crate) fn direct(
    service: &mut ApplicationService,
    input: &ApplicationInput,
) -> Result<GoldenReply, GoldenError> {
    let reply = service.execute(input);
    GoldenReply::from_reply(reply)
}

pub(crate) fn decode_reply(bytes: &[u8]) -> Result<GoldenReply, GoldenError> {
    serde_json::from_slice(bytes).map_err(GoldenError::Json)
}

pub(crate) fn decode_mcp(bytes: &[u8]) -> Result<GoldenMcpResponse, GoldenError> {
    serde_json::from_slice(bytes).map_err(GoldenError::Json)
}

#[derive(Debug)]
pub(crate) enum GoldenError {
    Input(InputTextError),
    Source(RejectedSourceText),
    Json(serde_json::Error),
    Unexpected(&'static str),
}

impl std::fmt::Display for GoldenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(error) => write!(formatter, "{error:?}"),
            Self::Source(error) => write!(formatter, "{error:?}"),
            Self::Json(error) => error.fmt(formatter),
            Self::Unexpected(law) => write!(formatter, "unexpected direct reply: {law}"),
        }
    }
}

impl std::error::Error for GoldenError {}

impl From<InputTextError> for GoldenError {
    fn from(error: InputTextError) -> Self {
        Self::Input(error)
    }
}

impl From<RejectedSourceText> for GoldenError {
    fn from(error: RejectedSourceText) -> Self {
        Self::Source(error)
    }
}

fn text(value: &str) -> Result<InputText, InputTextError> {
    InputText::try_from_str(value)
}

fn generate(
    correlation: u64,
    profile: LanguageProfile,
    stage: Stage,
    source: &str,
) -> Result<ApplicationInput, RejectedSourceText> {
    Ok(ApplicationInput::Generate(GenerateRequest {
        target: GenerateTarget {
            correlation: CorrelationId(correlation),
            profile,
            stage,
        },
        source: SourceText::try_from(source.to_owned())?,
    }))
}

fn budget() -> ResourceBudget {
    ResourceBudget {
        ram_free: ByteCount::from(4096),
        nvme_free: ByteCount::from(8192),
        operations: OperationBudget::from(1),
        retries: RetryBudget::from(1),
        memory_pressure: Pressure::Relaxed,
        storage_pressure: Pressure::Relaxed,
        battery: BatteryState::Normal,
    }
}

impl GoldenReply {
    fn from_reply(reply: ApplicationReply) -> Result<Self, GoldenError> {
        let correlation = reply.correlation.0;
        let (body, terminal, diagnostic) = match reply.outcome {
            ApplicationOutcome::Resolved(body) => {
                let terminal = match ApplicationDisposition::from(&body) {
                    ApplicationDisposition::Accepted { operation } => GoldenTerminal::Accepted {
                        operation: operation.0,
                    },
                    ApplicationDisposition::Complete { emitted } => {
                        GoldenTerminal::Complete { emitted }
                    }
                    ApplicationDisposition::Partial {
                        emitted,
                        unavailable,
                    } => GoldenTerminal::Partial {
                        emitted,
                        unavailable: unavailable.into(),
                    },
                    ApplicationDisposition::Degraded {
                        emitted,
                        unavailable,
                    } => GoldenTerminal::Degraded {
                        emitted,
                        unavailable: unavailable.into(),
                    },
                    ApplicationDisposition::Cancelled { emitted } => {
                        GoldenTerminal::Cancelled { emitted }
                    }
                };
                (project_body(&body)?, terminal, None)
            }
            ApplicationOutcome::Failed {
                diagnostic: failure,
            } => (
                GoldenBody::Rejected,
                GoldenTerminal::Failed,
                Some(diagnostic(failure)?),
            ),
        };
        Ok(Self {
            correlation,
            body,
            terminal,
            diagnostic,
        })
    }
}

fn project_body(body: &ReplyBody) -> Result<GoldenBody, GoldenError> {
    match *body {
        ReplyBody::Generated(artifact) => Ok(GoldenBody::Generated {
            artifact: generated_artifact(&artifact),
        }),
        ReplyBody::DependencyUnavailable { capability } => Ok(GoldenBody::DependencyUnavailable {
            capability: capability.into(),
        }),
        ReplyBody::Health(facts) => Ok(GoldenBody::Health {
            facts: facts.map(Into::into),
        }),
        ReplyBody::ExecutionStarted {
            operation,
            transition,
        } => Ok(GoldenBody::ExecutionStarted {
            operation: operation.0,
            transition: transition.into(),
        }),
        ReplyBody::Execution(state) => Ok(GoldenBody::Execution {
            state: execution_reply(state),
        }),
        ReplyBody::Adaptive(_) => Err(GoldenError::Unexpected("adaptive body")),
        ReplyBody::Snapshot(_) | ReplyBody::Retrieval(_) | ReplyBody::IndexRemoved(_) => {
            Err(GoldenError::Unexpected("retrieval body"))
        }
    }
}

fn execution_reply(state: ExecutionReply) -> GoldenExecutionState {
    match state {
        ExecutionReply::Pending {
            operation,
            transition,
        } => GoldenExecutionState::Pending {
            operation: operation.0,
            transition: transition.into(),
        },
        ExecutionReply::Completed {
            operation,
            transition,
        } => GoldenExecutionState::Completed {
            operation: operation.0,
            transition: transition.into(),
        },
        ExecutionReply::Cancelled {
            operation,
            transition,
        } => GoldenExecutionState::Cancelled {
            operation: operation.0,
            transition: transition.into(),
        },
    }
}

fn generated_artifact(artifact: &GeneratedArtifact) -> GoldenGeneratedArtifact {
    GoldenGeneratedArtifact {
        source: artifact.source.into(),
        recipe: artifact.recipe.into(),
        fragment: artifact.fragment.to_string(),
        publication: artifact.publication.into(),
    }
}

fn diagnostic(diagnostic: Diagnostic) -> Result<GoldenDiagnostic, GoldenError> {
    match diagnostic.detail {
        DiagnosticDetail::Capability(value) => Ok(GoldenDiagnostic {
            code: diagnostic.code.into(),
            detail: GoldenDiagnosticDetail::Capability {
                value: value.into(),
            },
        }),
        DiagnosticDetail::Compiler(terminal) => Ok(GoldenDiagnostic {
            code: diagnostic.code.into(),
            detail: GoldenDiagnosticDetail::Compiler {
                terminal: terminal.into(),
            },
        }),
        DiagnosticDetail::Execution(state) => Ok(GoldenDiagnostic {
            code: diagnostic.code.into(),
            detail: GoldenDiagnosticDetail::Execution {
                state: execution_state(state),
            },
        }),
        DiagnosticDetail::Retrieval(_) => Err(GoldenError::Unexpected("retrieval diagnostic")),
        DiagnosticDetail::Text(_)
        | DiagnosticDetail::Limit { .. }
        | DiagnosticDetail::TextLength { .. }
        | DiagnosticDetail::Frontend(_)
        | DiagnosticDetail::Operation(_)
        | DiagnosticDetail::Policy(_) => Err(GoldenError::Unexpected("non-capability diagnostic")),
    }
}

impl From<CapabilityHealth> for GoldenHealth {
    fn from(fact: CapabilityHealth) -> Self {
        match fact {
            CapabilityHealth::LocalReady(capability) => Self {
                capability: capability.into(),
                state: GoldenHealthState::LocalReady,
            },
            CapabilityHealth::Unavailable(capability) => Self {
                capability: capability.into(),
                state: GoldenHealthState::Unavailable,
            },
        }
    }
}

impl From<CapabilityTransition> for GoldenTransition {
    fn from(transition: CapabilityTransition) -> Self {
        match transition {
            CapabilityTransition::Acquire { capability, bundle } => Self::Acquire {
                capability: capability.into(),
                bundle: bundle.to_string(),
            },
            CapabilityTransition::Release { capability, bundle } => Self::Release {
                capability: capability.into(),
                bundle: bundle.to_string(),
            },
        }
    }
}

fn execution_state(state: ExecutionState) -> GoldenExecutionState {
    match state {
        ExecutionState::Pending {
            operation,
            transition,
        } => GoldenExecutionState::Pending {
            operation: operation.0,
            transition: transition.into(),
        },
        ExecutionState::Completed {
            operation,
            transition,
        } => GoldenExecutionState::Completed {
            operation: operation.0,
            transition: transition.into(),
        },
        ExecutionState::Cancelled {
            operation,
            transition,
        } => GoldenExecutionState::Cancelled {
            operation: operation.0,
            transition: transition.into(),
        },
        ExecutionState::Failed {
            operation,
            transition,
            phase,
        } => GoldenExecutionState::Failed {
            operation: operation.0,
            transition: transition.into(),
            phase: phase.into(),
        },
    }
}

impl From<heart_adaptive::ExecutionPhase> for GoldenExecutionPhase {
    fn from(phase: heart_adaptive::ExecutionPhase) -> Self {
        match phase {
            heart_adaptive::ExecutionPhase::LocalResidence => Self::LocalResidence,
            heart_adaptive::ExecutionPhase::CapabilityBundle => Self::CapabilityBundle,
            heart_adaptive::ExecutionPhase::Remote => Self::Remote,
        }
    }
}

impl From<Capability> for GoldenCapability {
    fn from(capability: Capability) -> Self {
        match capability {
            Capability::CompilerRegistry => Self::CompilerRegistry,
            Capability::CompilerOutput => Self::CompilerOutput,
            Capability::Index => Self::Index,
            Capability::Graph => Self::Graph,
            Capability::Vector => Self::Vector,
            Capability::LocalAnalyzer => Self::LocalAnalyzer,
            Capability::Remote => Self::Remote,
        }
    }
}

impl From<CapabilityKind> for GoldenCapabilityKind {
    fn from(capability: CapabilityKind) -> Self {
        match capability {
            CapabilityKind::Analyzer => Self::Analyzer,
            CapabilityKind::Compiler => Self::Compiler,
            CapabilityKind::Codec => Self::Codec,
            CapabilityKind::Model => Self::Model,
        }
    }
}

impl From<DiagnosticCode> for GoldenDiagnosticCode {
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
