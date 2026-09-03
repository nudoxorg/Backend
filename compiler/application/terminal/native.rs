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

use super::common::{
    attempt, authority_diagnostic, compile_from_driver, compiler_diagnostic, io_fact,
    source_authority,
};
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
        CompileFailure::Authority { source_identity, recipe, failure } => authority_terminal(source_identity, recipe, &failure),
        CompileFailure::AuthorityInputRequired { source_identity, recipe, .. }
        | CompileFailure::AuthorityInputProfileMismatch { source_identity, recipe, .. } => {
            compile_from_driver(
                source_identity,
                recipe,
                CompilerCause::Authority {
                    phase: compiler_vocabulary::AuthorityPhase::Open,
                    class: compiler_vocabulary::AuthorityDiagnosticClass::Authority,
                    diagnostic: None,
                },
            )
        }
        CompileFailure::LoweringUnsupported { source_identity, recipe, cause } => compile_from_driver(source_identity, recipe, CompilerCause::Lowering(cause)),
        CompileFailure::ExtensionAtomUnbound { source_identity, recipe, row, provisional, atom_count } => compile_from_driver(
            source_identity,
            recipe,
            CompilerCause::Lowering(compiler_vocabulary::LoweringUnsupported::ExtensionAtomUnbound {
                row: u32::try_from(row).unwrap_or(u32::MAX),
                provisional,
                atom_count: u32::try_from(atom_count).unwrap_or(u32::MAX),
            }),
        ),
        CompileFailure::FactRejected { source_identity, recipe, rejected } => compile_from_driver(
            source_identity,
            recipe,
            CompilerCause::Lowering(compiler_vocabulary::LoweringUnsupported::FactRejected {
                fact: u32::try_from(rejected.fact).unwrap_or(u32::MAX),
            }),
        ),
        CompileFailure::Build { source_identity, recipe, .. } | CompileFailure::Prepare { source_identity, recipe, .. } => fragment_terminal(source_identity, recipe, FragmentCause::Prepare),
        CompileFailure::Write { source_identity, recipe, .. } => fragment_terminal(source_identity, recipe, FragmentCause::Write),
        CompileFailure::Validate { source_identity, recipe, .. } => fragment_terminal(source_identity, recipe, FragmentCause::Validate),
    }
}

fn authority_terminal(
    source: compiler_ir::SourceIdentity,
    recipe: compiler_vocabulary::CompileRecipeFact,
    failure: &compiler_driver::AuthorityFailure<'_>,
) -> CompilerTerminal {
    let projection = failure.projection();
    compile_from_driver(
        source,
        recipe,
        CompilerCause::Authority {
            phase: projection.phase,
            class: projection.class,
            diagnostic: authority_diagnostic(projection.diagnostic),
        },
    )
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

#[cfg(test)]
mod tests {
    use compiler_driver::{AuthorityDiagnostic, AuthorityFailure};
    use compiler_languages_typescript::{AuthorityError, with_analysis};
    use compiler_vocabulary::{
        AuthorityDiagnosticClass, AuthorityPhase, CompileRecipeFact, LanguageProfile, NativeTool,
        Stage, TypeScriptSource,
    };
    use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};
    use interface_core::{CompilerCause, CompilerTerminal};

    use super::authority_terminal;

    #[derive(Debug, thiserror::Error)]
    enum ProjectionError {
        #[error("the terminal did not stay a compile terminal")]
        Terminal,
        #[error("the terminal cause was not the lowering cause")]
        Cause,
        #[error("the extension-atom projection lost its operands")]
        ExtensionOperands,
        #[error("the fact-rejection projection lost its ordinal")]
        FactOrdinal,
    }

    fn lowering_cause(
        terminal: CompilerTerminal,
    ) -> Result<compiler_vocabulary::LoweringUnsupported, ProjectionError> {
        let CompilerTerminal::Compile { cause, .. } = terminal else {
            return Err(ProjectionError::Terminal);
        };
        let CompilerCause::Lowering(cause) = cause else {
            return Err(ProjectionError::Cause);
        };
        Ok(cause)
    }

    fn fact_rejection() -> compiler_driver::FactRejection {
        compiler_driver::FactRejection {
            fact: 17,
            name_len: 6,
            cause: compiler_driver::FactFault::Capacity,
        }
    }

    fn source_and_recipe() -> (
        compiler_ir::SourceIdentity,
        compiler_vocabulary::CompileRecipeFact,
    ) {
        let source = compiler_ir::SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"projection"),
            byte_len: 10,
        };
        let recipe = compiler_vocabulary::CompileRecipeFact::derive(
            compiler_vocabulary::LanguageProfile::Python(
                compiler_vocabulary::PythonVersion::Python314,
            ),
            compiler_vocabulary::Stage::LowerIr,
            compiler_vocabulary::NativeTool::Python,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"projection-toolchain"),
        );
        (source, recipe)
    }

    #[test]
    fn extension_atom_unbound_projects_with_exact_operands() -> Result<(), ProjectionError> {
        let (source, recipe) = source_and_recipe();
        let failure = compiler_driver::CompileFailure::ExtensionAtomUnbound {
            source_identity: source,
            recipe,
            row: 9,
            provisional: 12,
            atom_count: 11,
        };
        let cause = lowering_cause(super::compile_terminal(failure))?;
        let compiler_vocabulary::LoweringUnsupported::ExtensionAtomUnbound {
            row,
            provisional,
            atom_count,
        } = cause
        else {
            return Err(ProjectionError::ExtensionOperands);
        };
        if row != 9 || provisional != 12 || atom_count != 11 {
            return Err(ProjectionError::ExtensionOperands);
        }
        Ok(())
    }

    #[test]
    fn fact_rejection_projects_with_the_exact_ordinal() -> Result<(), ProjectionError> {
        let (source, recipe) = source_and_recipe();
        let failure = compiler_driver::CompileFailure::FactRejected {
            source_identity: source,
            recipe,
            rejected: fact_rejection(),
        };
        let cause = lowering_cause(super::compile_terminal(failure))?;
        let compiler_vocabulary::LoweringUnsupported::FactRejected { fact } = cause else {
            return Err(ProjectionError::FactOrdinal);
        };
        if fact != 17 {
            return Err(ProjectionError::FactOrdinal);
        }
        Ok(())
    }

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("OXC accepted a syntactically malformed TypeScript fixture")]
        OxcAccepted,
        #[error("OXC returned a non-syntax terminal for malformed TypeScript")]
        OxcFailure,
        #[error("OXC syntax terminal had no retained diagnostics")]
        OxcDiagnostics,
        #[error("bounded authority diagnostic construction failed")]
        Diagnostic,
        #[error("authority terminal did not remain a compile terminal")]
        Terminal,
        #[error("authority terminal changed its source or recipe authority")]
        Attempt,
        #[error("authority terminal lost its OXC parse/syntax classification")]
        Classification,
        #[error("authority terminal lost its bounded primary diagnostic")]
        Projection,
    }

    #[test]
    fn actual_oxc_syntax_failure_projects_to_application_terminal() -> Result<(), TestError> {
        let source_bytes = b"const =;";
        let source_text = "const =;";
        let analyzed = with_analysis(TypeScriptSource::TypeScript, source_text, |_| ());
        let Err(cause) = analyzed else {
            return Err(TestError::OxcAccepted);
        };
        let AuthorityError::Syntax { diagnostics } = cause else {
            return Err(TestError::OxcFailure);
        };
        if diagnostics.is_empty() {
            return Err(TestError::OxcDiagnostics);
        }
        let Ok(diagnostic) = AuthorityDiagnostic::new(b"syntax", 6, false) else {
            return Err(TestError::Diagnostic);
        };
        let source = compiler_ir::SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
            byte_len: 8,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            Stage::LowerIr,
            NativeTool::TypeScriptCompiler,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"oxc-authority"),
        );
        let failure = AuthorityFailure::TypeScript {
            diagnostic,
            cause: AuthorityError::Syntax { diagnostics },
        };
        let terminal = authority_terminal(source, recipe, &failure);
        let CompilerTerminal::Compile { attempted, cause } = terminal else {
            return Err(TestError::Terminal);
        };
        if attempted.source.identity != source.identity
            || attempted.source.byte_len != source.byte_len
            || attempted.recipe != recipe.identity
        {
            return Err(TestError::Attempt);
        }
        let CompilerCause::Authority {
            phase,
            class,
            diagnostic,
        } = cause
        else {
            return Err(TestError::Terminal);
        };
        if phase != AuthorityPhase::Parse || class != AuthorityDiagnosticClass::Syntax {
            return Err(TestError::Classification);
        }
        let Some(diagnostic) = diagnostic else {
            return Err(TestError::Projection);
        };
        if diagnostic.byte_len != 6
            || diagnostic.observed != 6
            || diagnostic.truncated
            || diagnostic.bytes.get(..diagnostic.byte_len) != Some(b"syntax")
        {
            return Err(TestError::Projection);
        }
        Ok(())
    }
}
