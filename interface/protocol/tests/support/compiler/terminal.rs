//! Exercises the `interface-protocol` tests support compiler terminal contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::time::Duration;

use interface_core::{
    CompilerCause, CompilerDiagnostic, CompilerTerminal, FragmentCause, LoweringUnsupported,
};
use serde::Deserialize;

use super::authority::GoldenSourceAuthority;
use super::native::{GoldenNativeIoFact, GoldenNativeIoPhase, GoldenNativeWorkCause};
use super::publication::GoldenPublicationCause;

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenCompilerTerminal {
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
pub(crate) struct GoldenCompilerAttempt {
    pub source: GoldenSourceAuthority,
    pub recipe: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenCompilerCause {
    Authority {
        phase: GoldenAuthorityPhase,
        class: GoldenAuthorityDiagnosticClass,
        diagnostic: Option<GoldenCompilerDiagnostic>,
    },
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
pub(crate) enum GoldenLoweringCause {
    NoSupportedDeclaration,
    ExtensionAtomUnbound {
        row: u32,
        provisional: u32,
        atom_count: u32,
    },
    ExtensionTypeParametersUnbound {
        row: u32,
        start: u32,
        length: u32,
        element_count: u32,
    },
    FactRejected {
        fact: u32,
    },
    RustFunction,
    RustConstantType,
    RustGenericParameter,
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
    JavaProjection {
        class: String,
        declaration: String,
        owner: String,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenAuthorityPhase {
    Open,
    Parse,
    Resolve,
    TypeCheck,
    Project,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenAuthorityDiagnosticClass {
    Syntax,
    Binding,
    Type,
    Authority,
    Projection,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenFragmentCause {
    Prepare,
    Write,
    Validate,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenCompilerDiagnostic {
    pub byte_len: usize,
    pub observed: usize,
    pub truncated: bool,
    pub bytes: Vec<u8>,
}

impl From<CompilerTerminal> for GoldenCompilerTerminal {
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
            CompilerCause::Authority {
                phase,
                class,
                diagnostic,
            } => Self::Authority {
                phase: match phase {
                    compiler_vocabulary::AuthorityPhase::Open => GoldenAuthorityPhase::Open,
                    compiler_vocabulary::AuthorityPhase::Parse => GoldenAuthorityPhase::Parse,
                    compiler_vocabulary::AuthorityPhase::Resolve => GoldenAuthorityPhase::Resolve,
                    compiler_vocabulary::AuthorityPhase::TypeCheck => {
                        GoldenAuthorityPhase::TypeCheck
                    }
                    compiler_vocabulary::AuthorityPhase::Project => GoldenAuthorityPhase::Project,
                },
                class: match class {
                    compiler_vocabulary::AuthorityDiagnosticClass::Syntax => {
                        GoldenAuthorityDiagnosticClass::Syntax
                    }
                    compiler_vocabulary::AuthorityDiagnosticClass::Binding => {
                        GoldenAuthorityDiagnosticClass::Binding
                    }
                    compiler_vocabulary::AuthorityDiagnosticClass::Type => {
                        GoldenAuthorityDiagnosticClass::Type
                    }
                    compiler_vocabulary::AuthorityDiagnosticClass::Authority => {
                        GoldenAuthorityDiagnosticClass::Authority
                    }
                    compiler_vocabulary::AuthorityDiagnosticClass::Projection => {
                        GoldenAuthorityDiagnosticClass::Projection
                    }
                },
                diagnostic: diagnostic.map(Into::into),
            },
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
                cause: (*cause).into(),
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
            LoweringUnsupported::ExtensionAtomUnbound {
                row,
                provisional,
                atom_count,
            } => Self::ExtensionAtomUnbound {
                row,
                provisional,
                atom_count,
            },
            LoweringUnsupported::ExtensionTypeParametersUnbound {
                row,
                start,
                length,
                element_count,
            } => Self::ExtensionTypeParametersUnbound {
                row,
                start,
                length,
                element_count,
            },
            LoweringUnsupported::FactRejected { fact } => Self::FactRejected { fact },
            LoweringUnsupported::RustFunction => Self::RustFunction,
            LoweringUnsupported::RustConstantType => Self::RustConstantType,
            LoweringUnsupported::RustGenericParameter => Self::RustGenericParameter,
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
            LoweringUnsupported::JavaProjection {
                class,
                declaration,
                owner,
            } => Self::JavaProjection {
                class: match class {
                    compiler_vocabulary::JavaProjectionFaultClass::Image => "image",
                    compiler_vocabulary::JavaProjectionFaultClass::Depth => "depth",
                    compiler_vocabulary::JavaProjectionFaultClass::Malformed => "malformed",
                    compiler_vocabulary::JavaProjectionFaultClass::Primitive => "primitive",
                    compiler_vocabulary::JavaProjectionFaultClass::Utf8 => "utf8",
                    compiler_vocabulary::JavaProjectionFaultClass::SourceUtf8 => "source_utf8",
                    compiler_vocabulary::JavaProjectionFaultClass::Utf16 => "utf16",
                    compiler_vocabulary::JavaProjectionFaultClass::OrphanOwner => "orphan_owner",
                    compiler_vocabulary::JavaProjectionFaultClass::ForeignKey => "foreign_key",
                    compiler_vocabulary::JavaProjectionFaultClass::SiblingCapacity => {
                        "sibling_capacity"
                    }
                    compiler_vocabulary::JavaProjectionFaultClass::IndexCapacity => {
                        "index_capacity"
                    }
                }
                .to_owned(),
                declaration: declaration.to_string(),
                owner: owner.to_string(),
            },
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
