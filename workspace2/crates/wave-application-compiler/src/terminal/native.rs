//! Exhaustive native-driver terminal projection.

use nudox_compile_driver::{
    CompileFailure, LoweringUnsupported, NativeDiagnostic, NativeWorkError, NativeWorkPrimary,
    ToolchainSelectionFact,
};
use wave_application_core::{
    CompilerCause, CompilerTerminal, FragmentCause, LoweringCause, NativeDirectoryCause,
    NativeIoPhase, NativePrimaryCause, NativeWorkCause, NativeWorkCleanupCause, NativeWorkPhase,
};

use super::common::{attempt, compile_from_driver, diagnostic_copy, io_fact, source_authority};

/// Projects every borrowed driver terminal before its scratch lease expires.
#[rustfmt::skip]
pub(crate) fn compile_terminal(error: CompileFailure<'_>) -> CompilerTerminal {
    match error {
        CompileFailure::SourceLength { actual, .. } => CompilerTerminal::SourceLength { actual },
        CompileFailure::UnsupportedStage { source_identity, language, stage, .. } => unsupported_stage(source_identity, language, stage),
        CompileFailure::ToolchainSelectionMismatch { source_identity, language, stage, selected, provided } => toolchain(source_identity, language, stage, selected, configured_tool(provided)),
        CompileFailure::ToolchainMismatch { source_identity, language, stage, selected, resolved } => toolchain(source_identity, language, stage, selected, resolved),
        CompileFailure::ToolingUnavailable { source_identity, language, stage, tool } => tooling_unavailable(source_identity, language, stage, tool),
        CompileFailure::Cancelled { source_identity, recipe, diagnostic } => cancelled(source_identity, recipe, diagnostic),
        CompileFailure::NativeWork { source_identity, recipe, phase, cause } => native_work_terminal(source_identity, recipe, phase, cause),
        CompileFailure::NativeWorkCleanup { source_identity, recipe, primary, cleanup } => native_work_cleanup_terminal(source_identity, recipe, primary, cleanup),
        CompileFailure::ToolStart { source_identity, recipe, cause } => native_io_terminal(source_identity, recipe, NativeIoPhase::Start, &cause),
        CompileFailure::MissingToolInput { source_identity, recipe } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::MissingInput { cleanup: None }),
        CompileFailure::MissingToolInputCleanup { source_identity, recipe, cleanup } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::MissingInput { cleanup: Some(io_fact(&cleanup)) }),
        CompileFailure::MissingToolDiagnostic { source_identity, recipe } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::MissingDiagnostic { cleanup: None }),
        CompileFailure::MissingToolDiagnosticCleanup { source_identity, recipe, cleanup } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::MissingDiagnostic { cleanup: Some(io_fact(&cleanup)) }),
        CompileFailure::ToolInput { source_identity, recipe, cause } => native_io_terminal(source_identity, recipe, NativeIoPhase::Input, &cause),
        CompileFailure::ToolInputCleanup { source_identity, recipe, cause, cleanup } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::Input { cause: io_fact(&cause), cleanup: Some(io_fact(&cleanup)) }),
        CompileFailure::ToolTerminate { source_identity, recipe, cause } => native_io_terminal(source_identity, recipe, NativeIoPhase::Terminate, &cause),
        CompileFailure::ToolWait { source_identity, recipe, cause } => native_io_terminal(source_identity, recipe, NativeIoPhase::Wait, &cause),
        CompileFailure::ToolWaitCleanup { source_identity, recipe, cause, cleanup } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::Wait { cause: io_fact(&cause), cleanup: Some(io_fact(&cleanup)) }),
        CompileFailure::ToolDiagnosticRead { source_identity, recipe, cause } => native_io_terminal(source_identity, recipe, NativeIoPhase::DiagnosticRead, &cause),
        CompileFailure::DeadlineExceeded { source_identity, recipe, diagnostic } => deadline_exceeded(source_identity, recipe, diagnostic),
        CompileFailure::DiagnosticLimit { source_identity, recipe, limit, observed, diagnostic } => diagnostic_limit(source_identity, recipe, limit, observed, diagnostic),
        CompileFailure::NativeRejected { source_identity, recipe, status, diagnostic } => native_rejected(source_identity, recipe, status, diagnostic),
        CompileFailure::LoweringUnsupported { source_identity, recipe, cause } => compile_from_driver(source_identity, recipe, CompilerCause::Lowering(lowering(cause))),
        CompileFailure::Prepare { source_identity, recipe, .. } => fragment_terminal(source_identity, recipe, FragmentCause::Prepare),
        CompileFailure::Write { source_identity, recipe, .. } => fragment_terminal(source_identity, recipe, FragmentCause::Write),
        CompileFailure::Validate { source_identity, recipe, .. } => fragment_terminal(source_identity, recipe, FragmentCause::Validate),
    }
}

const fn unsupported_stage(
    source: nudox_ir_format::SourceIdentity,
    language: nudox_compile_vocab::Language,
    stage: nudox_compile_vocab::Stage,
) -> CompilerTerminal {
    CompilerTerminal::UnsupportedStage {
        source: source_authority(source),
        language,
        stage,
    }
}

const fn toolchain(
    source: nudox_ir_format::SourceIdentity,
    language: nudox_compile_vocab::Language,
    stage: nudox_compile_vocab::Stage,
    selected: nudox_compile_vocab::NativeTool,
    configured: nudox_compile_vocab::NativeTool,
) -> CompilerTerminal {
    CompilerTerminal::Toolchain {
        source: source_authority(source),
        language,
        stage,
        selected,
        configured: Some(configured),
    }
}

const fn tooling_unavailable(
    source: nudox_ir_format::SourceIdentity,
    language: nudox_compile_vocab::Language,
    stage: nudox_compile_vocab::Stage,
    tool: nudox_compile_vocab::NativeTool,
) -> CompilerTerminal {
    CompilerTerminal::ToolingUnavailable {
        source: source_authority(source),
        language,
        stage,
        tool,
    }
}

fn cancelled(
    source: nudox_ir_format::SourceIdentity,
    recipe: nudox_compile_vocab::CompileRecipeFact,
    diagnostic: NativeDiagnostic<'_>,
) -> CompilerTerminal {
    CompilerTerminal::Cancelled {
        attempted: attempt(source_authority(source), recipe),
        diagnostic: diagnostic_copy(diagnostic),
    }
}

fn native_work_terminal(
    source: nudox_ir_format::SourceIdentity,
    recipe: nudox_compile_vocab::CompileRecipeFact,
    phase: nudox_compile_driver::NativeWorkPhase,
    cause: NativeWorkError,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::NativeWork(native_work_cause(phase, cause)),
    )
}

fn native_work_cleanup_terminal(
    source: nudox_ir_format::SourceIdentity,
    recipe: nudox_compile_vocab::CompileRecipeFact,
    primary: NativeWorkPrimary<'_>,
    cleanup: NativeWorkError,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::NativeWork(NativeWorkCause::PrimaryAndCleanup {
            primary: native_primary(primary),
            cleanup: native_work_cleanup(cleanup),
        }),
    )
}

const fn native_primary_terminal(
    source: nudox_ir_format::SourceIdentity,
    recipe: nudox_compile_vocab::CompileRecipeFact,
    primary: NativePrimaryCause,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::NativeWork(NativeWorkCause::Primary(primary)),
    )
}

fn native_io_terminal(
    source: nudox_ir_format::SourceIdentity,
    recipe: nudox_compile_vocab::CompileRecipeFact,
    phase: NativeIoPhase,
    cause: &std::io::Error,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::NativeIo {
            phase,
            cause: io_fact(cause),
        },
    )
}

fn deadline_exceeded(
    source: nudox_ir_format::SourceIdentity,
    recipe: nudox_compile_vocab::CompileRecipeFact,
    diagnostic: NativeDiagnostic<'_>,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::DeadlineExceeded {
            diagnostic: diagnostic_copy(diagnostic),
        },
    )
}

fn diagnostic_limit(
    source: nudox_ir_format::SourceIdentity,
    recipe: nudox_compile_vocab::CompileRecipeFact,
    limit: usize,
    observed: usize,
    diagnostic: NativeDiagnostic<'_>,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::DiagnosticLimit {
            limit,
            observed,
            diagnostic: diagnostic_copy(diagnostic),
        },
    )
}

fn native_rejected(
    source: nudox_ir_format::SourceIdentity,
    recipe: nudox_compile_vocab::CompileRecipeFact,
    status: std::process::ExitStatus,
    diagnostic: NativeDiagnostic<'_>,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::NativeRejected {
            code: status.code(),
            diagnostic: diagnostic_copy(diagnostic),
        },
    )
}

const fn fragment_terminal(
    source: nudox_ir_format::SourceIdentity,
    recipe: nudox_compile_vocab::CompileRecipeFact,
    cause: FragmentCause,
) -> CompilerTerminal {
    compile_from_driver(source, recipe, CompilerCause::Fragment(cause))
}

const fn configured_tool(fact: ToolchainSelectionFact) -> nudox_compile_vocab::NativeTool {
    match fact {
        ToolchainSelectionFact::ResolvedNative { tool }
        | ToolchainSelectionFact::ExplicitlyUnavailable { tool } => tool,
    }
}

const fn lowering(cause: LoweringUnsupported) -> LoweringCause {
    match cause {
        LoweringUnsupported::NoSupportedDeclaration => LoweringCause::NoSupportedDeclaration,
        LoweringUnsupported::RustFunction => LoweringCause::RustFunction,
        LoweringUnsupported::RustConstantType => LoweringCause::RustConstantType,
        LoweringUnsupported::PythonAssignmentName => LoweringCause::PythonAssignmentName,
        LoweringUnsupported::PythonAssignmentValue => LoweringCause::PythonAssignmentValue,
        LoweringUnsupported::ClangDeclarationForm => LoweringCause::ClangDeclarationForm,
    }
}

fn native_work_cause(
    phase: nudox_compile_driver::NativeWorkPhase,
    cause: NativeWorkError,
) -> NativeWorkCause {
    NativeWorkCause::Directory {
        phase: match phase {
            nudox_compile_driver::NativeWorkPhase::Prepare => NativeWorkPhase::Prepare,
            nudox_compile_driver::NativeWorkPhase::Cleanup => NativeWorkPhase::Cleanup,
        },
        cause: match cause {
            NativeWorkError::RelativeDirectory => NativeDirectoryCause::RelativeDirectory,
            NativeWorkError::NotEmpty => NativeDirectoryCause::NotEmpty,
            NativeWorkError::Inspect(cause) => NativeDirectoryCause::InspectIo(io_fact(&cause)),
            NativeWorkError::RemoveMetadata(cause) => {
                NativeDirectoryCause::RemoveMetadataIo(io_fact(&cause))
            }
        },
    }
}

fn native_work_cleanup(cause: NativeWorkError) -> NativeWorkCleanupCause {
    match cause {
        NativeWorkError::RelativeDirectory => NativeWorkCleanupCause::RelativeDirectory,
        NativeWorkError::NotEmpty => NativeWorkCleanupCause::NotEmpty,
        NativeWorkError::Inspect(cause) | NativeWorkError::RemoveMetadata(cause) => {
            NativeWorkCleanupCause::Io(io_fact(&cause))
        }
    }
}

fn native_primary(primary: NativeWorkPrimary<'_>) -> NativePrimaryCause {
    match primary {
        NativeWorkPrimary::ToolStart { cause } => NativePrimaryCause::ToolStart(io_fact(&cause)),
        NativeWorkPrimary::MissingToolInput => NativePrimaryCause::MissingInput { cleanup: None },
        NativeWorkPrimary::MissingToolInputCleanup { cleanup } => {
            NativePrimaryCause::MissingInput {
                cleanup: Some(io_fact(&cleanup)),
            }
        }
        NativeWorkPrimary::MissingToolDiagnostic => {
            NativePrimaryCause::MissingDiagnostic { cleanup: None }
        }
        NativeWorkPrimary::MissingToolDiagnosticCleanup { cleanup } => {
            NativePrimaryCause::MissingDiagnostic {
                cleanup: Some(io_fact(&cleanup)),
            }
        }
        NativeWorkPrimary::ToolInput { cause } => NativePrimaryCause::Input {
            cause: io_fact(&cause),
            cleanup: None,
        },
        NativeWorkPrimary::ToolInputCleanup { cause, cleanup } => NativePrimaryCause::Input {
            cause: io_fact(&cause),
            cleanup: Some(io_fact(&cleanup)),
        },
        NativeWorkPrimary::ToolTerminate { cause } => {
            NativePrimaryCause::Terminate(io_fact(&cause))
        }
        NativeWorkPrimary::ToolWait { cause } => NativePrimaryCause::Wait {
            cause: io_fact(&cause),
            cleanup: None,
        },
        NativeWorkPrimary::ToolWaitCleanup { cause, cleanup } => NativePrimaryCause::Wait {
            cause: io_fact(&cause),
            cleanup: Some(io_fact(&cleanup)),
        },
        NativeWorkPrimary::ToolDiagnosticRead { cause } => {
            NativePrimaryCause::DiagnosticRead(io_fact(&cause))
        }
        NativeWorkPrimary::Cancelled { diagnostic } => {
            NativePrimaryCause::Cancelled(diagnostic_copy(diagnostic))
        }
        NativeWorkPrimary::DeadlineExceeded { diagnostic } => {
            NativePrimaryCause::DeadlineExceeded(diagnostic_copy(diagnostic))
        }
        NativeWorkPrimary::DiagnosticLimit {
            limit,
            observed,
            diagnostic,
        } => NativePrimaryCause::DiagnosticLimit {
            limit,
            observed,
            diagnostic: diagnostic_copy(diagnostic),
        },
        NativeWorkPrimary::NativeRejected { status, diagnostic } => NativePrimaryCause::Rejected {
            code: status.code(),
            diagnostic: diagnostic_copy(diagnostic),
        },
    }
}
