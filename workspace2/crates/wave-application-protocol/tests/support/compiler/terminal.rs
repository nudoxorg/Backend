use std::time::Duration;

use serde::Deserialize;
use wave_application_core::{
    CompilerCause, CompilerDiagnostic, CompilerTerminal, FragmentCause, LoweringUnsupported,
};

use super::authority::GoldenSourceAuthority;
use super::native::{GoldenNativeIoFact, GoldenNativeIoPhase, GoldenNativeWorkCause};
use super::publication::GoldenPublicationCause;

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoldenCompilerTerminal {
    SourceLength {
        actual: usize,
    },
    Unavailable {
        language: super::authority::GoldenLanguage,
        stage: super::authority::GoldenStage,
    },
    UnsupportedStage {
        source: GoldenSourceAuthority,
        language: super::authority::GoldenLanguage,
        stage: super::authority::GoldenStage,
    },
    DeadlineConstruction {
        source: GoldenSourceAuthority,
        language: super::authority::GoldenLanguage,
        stage: super::authority::GoldenStage,
        timeout: Duration,
    },
    Toolchain {
        source: GoldenSourceAuthority,
        language: super::authority::GoldenLanguage,
        stage: super::authority::GoldenStage,
        selected: super::authority::GoldenNativeTool,
        configured: Option<super::authority::GoldenNativeTool>,
    },
    ToolingUnavailable {
        source: GoldenSourceAuthority,
        language: super::authority::GoldenLanguage,
        stage: super::authority::GoldenStage,
        tool: super::authority::GoldenNativeTool,
    },
    Cancelled {
        attempted: GoldenCompilerAttempt,
        diagnostic: Option<GoldenCompilerDiagnostic>,
    },
    Compile {
        attempted: GoldenCompilerAttempt,
        cause: GoldenCompilerCause,
    },
    Publication {
        attempted: GoldenCompilerAttempt,
        cause: GoldenPublicationCause,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct GoldenCompilerAttempt {
    pub source: GoldenSourceAuthority,
    pub recipe: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoldenCompilerCause {
    NativeWork {
        cause: GoldenNativeWorkCause,
    },
    NativeIo {
        phase: GoldenNativeIoPhase,
        cause: GoldenNativeIoFact,
    },
    NativeRejected {
        code: Option<i32>,
        diagnostic: Option<GoldenCompilerDiagnostic>,
    },
    DeadlineExceeded {
        diagnostic: Option<GoldenCompilerDiagnostic>,
    },
    DiagnosticLimit {
        limit: usize,
        observed: usize,
        diagnostic: Option<GoldenCompilerDiagnostic>,
    },
    Lowering {
        cause: GoldenLoweringCause,
    },
    Fragment {
        cause: GoldenFragmentCause,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GoldenLoweringCause {
    NoSupportedDeclaration,
    RustFunction,
    RustConstantType,
    PythonAssignmentName,
    PythonAssignmentValue,
    ClangDeclarationForm,
    TypeScriptDeclarationForm,
    TypeScriptDeclarationType,
    CSharpDeclarationForm,
    CSharpDeclarationType,
    GoDeclarationForm,
    GoDeclarationType,
    JavaDeclarationForm,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GoldenFragmentCause {
    Prepare,
    Write,
    Validate,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct GoldenCompilerDiagnostic {
    pub byte_len: usize,
    pub observed: usize,
    pub truncated: bool,
    pub bytes: Vec<u8>,
}

impl From<CompilerTerminal> for GoldenCompilerTerminal {
    #[allow(clippy::too_many_lines)]
    fn from(terminal: CompilerTerminal) -> Self {
        match terminal {
            CompilerTerminal::SourceLength { actual } => Self::SourceLength { actual },
            CompilerTerminal::Unavailable { language, stage } => Self::Unavailable {
                language: language.into(),
                stage: stage.into(),
            },
            CompilerTerminal::UnsupportedStage {
                source,
                language,
                stage,
            } => Self::UnsupportedStage {
                source: source.into(),
                language: language.into(),
                stage: stage.into(),
            },
            CompilerTerminal::DeadlineConstruction {
                source,
                language,
                stage,
                timeout,
            } => Self::DeadlineConstruction {
                source: source.into(),
                language: language.into(),
                stage: stage.into(),
                timeout,
            },
            CompilerTerminal::Toolchain {
                source,
                language,
                stage,
                selected,
                configured,
            } => Self::Toolchain {
                source: source.into(),
                language: language.into(),
                stage: stage.into(),
                selected: selected.into(),
                configured: configured.map(Into::into),
            },
            CompilerTerminal::ToolingUnavailable {
                source,
                language,
                stage,
                tool,
            } => Self::ToolingUnavailable {
                source: source.into(),
                language: language.into(),
                stage: stage.into(),
                tool: tool.into(),
            },
            CompilerTerminal::Cancelled {
                attempted,
                diagnostic,
            } => Self::Cancelled {
                attempted: attempted.into(),
                diagnostic: diagnostic.map(Into::into),
            },
            CompilerTerminal::Compile { attempted, cause } => Self::Compile {
                attempted: attempted.into(),
                cause: cause.into(),
            },
            CompilerTerminal::Publication { attempted, cause } => Self::Publication {
                attempted: attempted.into(),
                cause: cause.into(),
            },
        }
    }
}

impl From<CompilerCause> for GoldenCompilerCause {
    fn from(cause: CompilerCause) -> Self {
        match cause {
            CompilerCause::NativeWork(cause) => Self::NativeWork {
                cause: cause.into(),
            },
            CompilerCause::NativeIo { phase, cause } => Self::NativeIo {
                phase: phase.into(),
                cause: cause.into(),
            },
            CompilerCause::NativeRejected { code, diagnostic } => Self::NativeRejected {
                code,
                diagnostic: diagnostic.map(Into::into),
            },
            CompilerCause::DeadlineExceeded { diagnostic } => Self::DeadlineExceeded {
                diagnostic: diagnostic.map(Into::into),
            },
            CompilerCause::DiagnosticLimit {
                limit,
                observed,
                diagnostic,
            } => Self::DiagnosticLimit {
                limit,
                observed,
                diagnostic: diagnostic.map(Into::into),
            },
            CompilerCause::Lowering(cause) => Self::Lowering {
                cause: cause.into(),
            },
            CompilerCause::Fragment(cause) => Self::Fragment {
                cause: cause.into(),
            },
        }
    }
}

impl From<LoweringUnsupported> for GoldenLoweringCause {
    fn from(cause: LoweringUnsupported) -> Self {
        match cause {
            LoweringUnsupported::NoSupportedDeclaration => Self::NoSupportedDeclaration,
            LoweringUnsupported::RustFunction => Self::RustFunction,
            LoweringUnsupported::RustConstantType => Self::RustConstantType,
            LoweringUnsupported::PythonAssignmentName => Self::PythonAssignmentName,
            LoweringUnsupported::PythonAssignmentValue => Self::PythonAssignmentValue,
            LoweringUnsupported::ClangDeclarationForm => Self::ClangDeclarationForm,
            LoweringUnsupported::TypeScriptDeclarationForm => Self::TypeScriptDeclarationForm,
            LoweringUnsupported::TypeScriptDeclarationType => Self::TypeScriptDeclarationType,
            LoweringUnsupported::CSharpDeclarationForm => Self::CSharpDeclarationForm,
            LoweringUnsupported::CSharpDeclarationType => Self::CSharpDeclarationType,
            LoweringUnsupported::GoDeclarationForm => Self::GoDeclarationForm,
            LoweringUnsupported::GoDeclarationType => Self::GoDeclarationType,
            LoweringUnsupported::JavaDeclarationForm => Self::JavaDeclarationForm,
        }
    }
}

impl From<FragmentCause> for GoldenFragmentCause {
    fn from(cause: FragmentCause) -> Self {
        match cause {
            FragmentCause::Prepare => Self::Prepare,
            FragmentCause::Write => Self::Write,
            FragmentCause::Validate => Self::Validate,
        }
    }
}

impl From<CompilerDiagnostic> for GoldenCompilerDiagnostic {
    fn from(diagnostic: CompilerDiagnostic) -> Self {
        Self {
            byte_len: diagnostic.byte_len,
            observed: diagnostic.observed,
            truncated: diagnostic.truncated,
            bytes: diagnostic.bytes[..diagnostic.byte_len].to_vec(),
        }
    }
}
