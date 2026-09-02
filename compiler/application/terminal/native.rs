//! Defines terminal native behavior for `compiler-application`, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the terminal native invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Exhaustive native-driver terminal projection.

mod work;

use compiler_driver::{
    CompileFailure, NativeDiagnostic, NativeWorkError, NativeWorkPrimary, ToolchainSelectionFact,
};
use interface_core::{
    CompilerCause, CompilerTerminal, FragmentCause, NativeIoPhase, NativePrimaryCause,
    NativeWorkCause, NativeWorkPhase,
};

use super::common::{attempt, compile_from_driver, compiler_diagnostic, io_fact, source_authority};
use work::{native_primary, native_work_cause, native_work_cleanup};

/// Projects every borrowed driver terminal before its scratch lease expires.
#[rustfmt::skip]
pub(crate) fn compile_terminal(error: CompileFailure<'_>) -> CompilerTerminal {
    match error {
        CompileFailure::SourceLength { actual, .. } => CompilerTerminal::SourceLength { actual },
        CompileFailure::UnsupportedStage { source_identity, language, stage, .. } => unsupported_stage(source_identity, language, stage),
        CompileFailure::ToolchainSelectionMismatch { source_identity, language, stage, selected, provided } => toolchain(source_identity, language, stage, selected, configured_tool(provided)),
        CompileFailure::ToolchainMismatch { source_identity, language, stage, selected, resolved } => toolchain(source_identity, language, stage, selected, Some(resolved)),
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
        CompileFailure::ToolDiagnosticReadCleanup { source_identity, recipe, cause, cleanup } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::DiagnosticRead { cause: io_fact(&cause), cleanup: Some(io_fact(&cleanup)) }),
        CompileFailure::NativeWorkerPanic { source_identity, recipe, cause } => native_primary_terminal(source_identity, recipe, NativePrimaryCause::WorkerPanic(cause)),
        CompileFailure::DeadlineExceeded { source_identity, recipe, diagnostic } => deadline_exceeded(source_identity, recipe, diagnostic),
        CompileFailure::DiagnosticLimit { source_identity, recipe, limit, observed, diagnostic } => diagnostic_limit(source_identity, recipe, limit, observed, diagnostic),
        CompileFailure::NativeRejected { source_identity, recipe, status, diagnostic } => native_rejected(source_identity, recipe, status, diagnostic),
        CompileFailure::LoweringUnsupported { source_identity, recipe, cause } => compile_from_driver(source_identity, recipe, CompilerCause::Lowering(cause)),
        CompileFailure::Prepare { source_identity, recipe, .. } => fragment_terminal(source_identity, recipe, FragmentCause::Prepare),
        CompileFailure::Write { source_identity, recipe, .. } => fragment_terminal(source_identity, recipe, FragmentCause::Write),
        CompileFailure::Validate { source_identity, recipe, .. } => fragment_terminal(source_identity, recipe, FragmentCause::Validate),
    }
}

const fn unsupported_stage(
    source: compiler_ir::SourceIdentity,
    language: compiler_vocabulary::Language,
    stage: compiler_vocabulary::Stage,
) -> CompilerTerminal {
    CompilerTerminal::UnsupportedStage {
        source: source_authority(source),
        language,
        stage,
    }
}

const fn toolchain(
    source: compiler_ir::SourceIdentity,
    language: compiler_vocabulary::Language,
    stage: compiler_vocabulary::Stage,
    selected: compiler_vocabulary::NativeTool,
    configured: Option<compiler_vocabulary::NativeTool>,
) -> CompilerTerminal {
    CompilerTerminal::Toolchain {
        source: source_authority(source),
        language,
        stage,
        selected,
        configured,
    }
}

const fn tooling_unavailable(
    source: compiler_ir::SourceIdentity,
    language: compiler_vocabulary::Language,
    stage: compiler_vocabulary::Stage,
    tool: compiler_vocabulary::NativeTool,
) -> CompilerTerminal {
    CompilerTerminal::ToolingUnavailable {
        source: source_authority(source),
        language,
        stage,
        tool,
    }
}

fn cancelled(
    source: compiler_ir::SourceIdentity,
    recipe: compiler_vocabulary::CompileRecipeFact,
    diagnostic: NativeDiagnostic<'_>,
) -> CompilerTerminal {
    CompilerTerminal::Cancelled {
        attempted: attempt(source_authority(source), recipe),
        diagnostic: compiler_diagnostic(diagnostic),
    }
}

fn native_work_terminal(
    source: compiler_ir::SourceIdentity,
    recipe: compiler_vocabulary::CompileRecipeFact,
    phase: NativeWorkPhase,
    cause: NativeWorkError,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::NativeWork(native_work_cause(phase, cause)),
    )
}

fn native_work_cleanup_terminal(
    source: compiler_ir::SourceIdentity,
    recipe: compiler_vocabulary::CompileRecipeFact,
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
    source: compiler_ir::SourceIdentity,
    recipe: compiler_vocabulary::CompileRecipeFact,
    primary: NativePrimaryCause,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::NativeWork(NativeWorkCause::Primary(primary)),
    )
}

fn native_io_terminal(
    source: compiler_ir::SourceIdentity,
    recipe: compiler_vocabulary::CompileRecipeFact,
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
    source: compiler_ir::SourceIdentity,
    recipe: compiler_vocabulary::CompileRecipeFact,
    diagnostic: NativeDiagnostic<'_>,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::DeadlineExceeded {
            diagnostic: compiler_diagnostic(diagnostic),
        },
    )
}

fn diagnostic_limit(
    source: compiler_ir::SourceIdentity,
    recipe: compiler_vocabulary::CompileRecipeFact,
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
            diagnostic: compiler_diagnostic(diagnostic),
        },
    )
}

fn native_rejected(
    source: compiler_ir::SourceIdentity,
    recipe: compiler_vocabulary::CompileRecipeFact,
    status: std::process::ExitStatus,
    diagnostic: NativeDiagnostic<'_>,
) -> CompilerTerminal {
    compile_from_driver(
        source,
        recipe,
        CompilerCause::NativeRejected {
            code: status.code(),
            diagnostic: compiler_diagnostic(diagnostic),
        },
    )
}

const fn fragment_terminal(
    source: compiler_ir::SourceIdentity,
    recipe: compiler_vocabulary::CompileRecipeFact,
    cause: FragmentCause,
) -> CompilerTerminal {
    compile_from_driver(source, recipe, CompilerCause::Fragment(cause))
}

const fn configured_tool(fact: ToolchainSelectionFact) -> Option<compiler_vocabulary::NativeTool> {
    match fact {
        ToolchainSelectionFact::ResolvedNative { tool } => Some(tool),
        ToolchainSelectionFact::ExplicitlyUnavailable { .. } => None,
    }
}
