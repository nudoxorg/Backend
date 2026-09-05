//! Exercises the `interface-protocol` tests support compiler terminal contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{path::PathBuf, time::Duration};

use interface_core::{
    CompilerCause, CompilerDiagnostic, CompilerRuntimeCause, CompilerTerminal, FragmentCause,
    LoweringUnsupported, PackageCompilePhase, PackageDeclarationScopeCause, PackageEcosystem,
    PackagePathComponentError, PackageSourceCause, PackageSourceIoPhase, PackageTextRange,
};
use serde::Deserialize;

use super::authority::GoldenSourceAuthority;
use super::native::{
    GoldenErrorKind, GoldenNativeIoFact, GoldenNativeIoPhase, GoldenNativeWorkCause,
    GoldenNativeWorkerPanicClass, GoldenNativeWorkerPanicMessage,
};
use super::publication::GoldenPublicationCause;

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenCompilerTerminal {
    PackageSource {
        target: String,
        phase: GoldenPackageCompilePhase,
        cause: GoldenPackageSourceCause,
    },
    PackageCancelled {
        target: String,
        phase: GoldenPackageCompilePhase,
    },
    Runtime {
        language: super::authority::GoldenLanguage,
        stage: super::authority::GoldenStage,
        target: Option<String>,
        cause: GoldenCompilerRuntimeCause,
    },
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
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenPackageCompilePhase {
    Locate,
    EnterSource,
    Authority,
    Lower,
    Publish,
    Reopen,
    Discover,
    Render,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenPackageEcosystem {
    Cargo,
    Npm,
    Pypi,
    Golang,
    Maven,
    Nuget,
    Generic,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenPackageTextRange {
    pub start: u16,
    pub end: u16,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenPackagePathComponentError {
    Traversal,
    EncodedSeparator,
    InvalidUtf8,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenPackageDeclarationScopeCause {
    EmptyEcosystem,
    EmptyPackage,
    EcosystemSeparator,
    PackageSeparator,
    LineageBackslash { segment: u8 },
    EmptySourcePath,
    SourceBackslash,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenPackageSourceIoPhase {
    CanonicalizeStore,
    EnumerateRegistry,
    CanonicalizePackage,
    CanonicalizeSource,
    SourceMetadata,
    ReadSource,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenPackageSourceIoFact {
    pub kind: GoldenErrorKind,
    pub raw_os_code: Option<i32>,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenPackageSourceCause {
    RootUnavailable {
        ecosystem: GoldenPackageEcosystem,
    },
    SubpathRequired {
        ecosystem: GoldenPackageEcosystem,
    },
    InvalidComponent {
        range: GoldenPackageTextRange,
        cause: GoldenPackagePathComponentError,
    },
    DeclarationScope {
        cause: GoldenPackageDeclarationScopeCause,
    },
    InvalidUtf8 {
        valid_up_to: usize,
        error_len: Option<u8>,
    },
    PackageUnavailable {
        path: PathBuf,
    },
    PackageEscapesStore {
        store: PathBuf,
        package: PathBuf,
    },
    SourceUnavailable {
        path: PathBuf,
    },
    SourceEscapesPackage {
        package: PathBuf,
        source: PathBuf,
    },
    SourceTooLarge {
        observed: u64,
        maximum: u64,
    },
    Io {
        phase: GoldenPackageSourceIoPhase,
        path: PathBuf,
        source: GoldenPackageSourceIoFact,
    },
    RegistryNamespaceCapacity {
        observed: usize,
        maximum: usize,
    },
    RegistryNamespaceAmbiguous {
        first: PathBuf,
        second: PathBuf,
    },
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenCompilerRuntimeCause {
    RequestInFlight,
    RequestOwnerStopped,
    ResponseOwnerStopped,
    WorkerPanic {
        class: GoldenNativeWorkerPanicClass,
        message: GoldenNativeWorkerPanicMessage,
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
            CompilerTerminal::PackageSource {
                target,
                phase,
                cause,
            } => Self::PackageSource {
                target: target.to_string(),
                phase: phase.into(),
                cause: cause.into(),
            },
            CompilerTerminal::PackageCancelled { target, phase } => Self::PackageCancelled {
                target: target.to_string(),
                phase: phase.into(),
            },
            CompilerTerminal::Runtime {
                language,
                stage,
                target,
                cause,
            } => Self::Runtime {
                language: language.into(),
                stage: stage.into(),
                target: target.map(|target| target.to_string()),
                cause: cause.into(),
            },
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

impl From<PackageCompilePhase> for GoldenPackageCompilePhase {
    fn from(phase: PackageCompilePhase) -> Self {
        match phase {
            PackageCompilePhase::Locate => Self::Locate,
            PackageCompilePhase::EnterSource => Self::EnterSource,
            PackageCompilePhase::Authority => Self::Authority,
            PackageCompilePhase::Lower => Self::Lower,
            PackageCompilePhase::Publish => Self::Publish,
            PackageCompilePhase::Reopen => Self::Reopen,
            PackageCompilePhase::Discover => Self::Discover,
            PackageCompilePhase::Render => Self::Render,
        }
    }
}

impl From<PackageEcosystem> for GoldenPackageEcosystem {
    fn from(ecosystem: PackageEcosystem) -> Self {
        match ecosystem {
            PackageEcosystem::Cargo => Self::Cargo,
            PackageEcosystem::Npm => Self::Npm,
            PackageEcosystem::Pypi => Self::Pypi,
            PackageEcosystem::Golang => Self::Golang,
            PackageEcosystem::Maven => Self::Maven,
            PackageEcosystem::Nuget => Self::Nuget,
            PackageEcosystem::Generic => Self::Generic,
        }
    }
}

impl From<PackageTextRange> for GoldenPackageTextRange {
    fn from(range: PackageTextRange) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }
}

impl From<PackagePathComponentError> for GoldenPackagePathComponentError {
    fn from(cause: PackagePathComponentError) -> Self {
        match cause {
            PackagePathComponentError::Traversal => Self::Traversal,
            PackagePathComponentError::EncodedSeparator => Self::EncodedSeparator,
            PackagePathComponentError::InvalidUtf8 => Self::InvalidUtf8,
        }
    }
}

impl From<PackageDeclarationScopeCause> for GoldenPackageDeclarationScopeCause {
    fn from(cause: PackageDeclarationScopeCause) -> Self {
        match cause {
            PackageDeclarationScopeCause::EmptyEcosystem => Self::EmptyEcosystem,
            PackageDeclarationScopeCause::EmptyPackage => Self::EmptyPackage,
            PackageDeclarationScopeCause::EcosystemSeparator => Self::EcosystemSeparator,
            PackageDeclarationScopeCause::PackageSeparator => Self::PackageSeparator,
            PackageDeclarationScopeCause::LineageBackslash { segment } => {
                Self::LineageBackslash { segment }
            }
            PackageDeclarationScopeCause::EmptySourcePath => Self::EmptySourcePath,
            PackageDeclarationScopeCause::SourceBackslash => Self::SourceBackslash,
        }
    }
}

impl From<PackageSourceIoPhase> for GoldenPackageSourceIoPhase {
    fn from(phase: PackageSourceIoPhase) -> Self {
        match phase {
            PackageSourceIoPhase::CanonicalizeStore => Self::CanonicalizeStore,
            PackageSourceIoPhase::EnumerateRegistry => Self::EnumerateRegistry,
            PackageSourceIoPhase::CanonicalizePackage => Self::CanonicalizePackage,
            PackageSourceIoPhase::CanonicalizeSource => Self::CanonicalizeSource,
            PackageSourceIoPhase::SourceMetadata => Self::SourceMetadata,
            PackageSourceIoPhase::ReadSource => Self::ReadSource,
        }
    }
}

impl From<PackageSourceCause> for GoldenPackageSourceCause {
    fn from(cause: PackageSourceCause) -> Self {
        match cause {
            PackageSourceCause::RootUnavailable { ecosystem } => Self::RootUnavailable {
                ecosystem: ecosystem.into(),
            },
            PackageSourceCause::SubpathRequired { ecosystem } => Self::SubpathRequired {
                ecosystem: ecosystem.into(),
            },
            PackageSourceCause::InvalidComponent { range, cause } => Self::InvalidComponent {
                range: range.into(),
                cause: cause.into(),
            },
            PackageSourceCause::DeclarationScope { cause } => Self::DeclarationScope {
                cause: cause.into(),
            },
            PackageSourceCause::InvalidUtf8 {
                valid_up_to,
                error_len,
            } => Self::InvalidUtf8 {
                valid_up_to,
                error_len,
            },
            PackageSourceCause::PackageUnavailable { path } => Self::PackageUnavailable {
                path: path.into_path_buf(),
            },
            PackageSourceCause::PackageEscapesStore { store, package } => {
                Self::PackageEscapesStore {
                    store: store.into_path_buf(),
                    package: package.into_path_buf(),
                }
            }
            PackageSourceCause::SourceUnavailable { path } => Self::SourceUnavailable {
                path: path.into_path_buf(),
            },
            PackageSourceCause::SourceEscapesPackage { package, source } => {
                Self::SourceEscapesPackage {
                    package: package.into_path_buf(),
                    source: source.into_path_buf(),
                }
            }
            PackageSourceCause::SourceTooLarge { observed, maximum } => {
                Self::SourceTooLarge { observed, maximum }
            }
            PackageSourceCause::Io {
                phase,
                path,
                source,
            } => Self::Io {
                phase: phase.into(),
                path: path.into_path_buf(),
                source: GoldenPackageSourceIoFact {
                    kind: source.kind.into(),
                    raw_os_code: source.raw_os_code,
                },
            },
            PackageSourceCause::RegistryNamespaceCapacity { observed, maximum } => {
                Self::RegistryNamespaceCapacity { observed, maximum }
            }
            PackageSourceCause::RegistryNamespaceAmbiguous { first, second } => {
                Self::RegistryNamespaceAmbiguous {
                    first: first.into_path_buf(),
                    second: second.into_path_buf(),
                }
            }
        }
    }
}

impl From<CompilerRuntimeCause> for GoldenCompilerRuntimeCause {
    fn from(cause: CompilerRuntimeCause) -> Self {
        match cause {
            CompilerRuntimeCause::RequestInFlight => Self::RequestInFlight,
            CompilerRuntimeCause::RequestOwnerStopped => Self::RequestOwnerStopped,
            CompilerRuntimeCause::ResponseOwnerStopped => Self::ResponseOwnerStopped,
            CompilerRuntimeCause::WorkerPanic(panic) => Self::WorkerPanic {
                class: panic.class.into(),
                message: panic.message.into(),
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
