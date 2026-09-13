//! Exercises the `compiler-application` tests local-compiler journey contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::sync::atomic::{AtomicBool, Ordering};

use compiler_vocabulary::{
    Language, LanguageProfile, PythonVersion, RustEdition, Stage, TypeScriptSource,
};
use backend_version::{ContentId, SourceFactDomain};
use interface_core::{
    ApplicationDisposition, ApplicationOutcome, ApplicationReply, ApplicationService,
    CompilerTerminal, Diagnostic, DiagnosticCode, DiagnosticDetail, ReplyBody,
};

use super::support::{
    Fixture, LocalCompilerTestError, generate, open_local_compiler, python_path, python_toolchains,
};

#[test]
fn configured_python_compiler_lowers_publishes_and_preserves_exact_terminals()
-> Result<(), LocalCompilerTestError> {
    let fixture = Fixture::create()?;
    let cancelled = AtomicBool::new(false);
    let mut scratch = compiler_application::LocalCompilerScratch::default();
    let python = python_path()?;
    let toolchains = python_toolchains(&python)?;
    let compiler = open_local_compiler(&fixture, &toolchains, &cancelled, &mut scratch)?;
    let mut service = ApplicationService::with_compiler(compiler);

    assert_generated(service.execute(&generate(
        LanguageProfile::Python(PythonVersion::Python314),
        Stage::LowerIr,
        "ready = 1",
    )?))?;
    // A second generation with changed content chains onto the same journal
    // (parent linkage); it succeeds instead of conflicting with the head —
    // the durable-conflict terminal remains reserved for same-chain-position
    // divergence, proven at the publication layer.
    assert_generated(service.execute(&generate(
        LanguageProfile::Python(PythonVersion::Python314),
        Stage::LowerIr,
        "ready = 2",
    )?))?;
    assert_missing_native_toolchain(service.execute(&generate(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        "pub const READY: i32 = 1;",
    )?))?;
    assert_explicitly_unavailable_tool(service.execute(&generate(
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        Stage::LowerIr,
        "export const READY = 1;",
    )?))?;
    assert_unsupported_stage(service.execute(&generate(
        LanguageProfile::Python(PythonVersion::Python314),
        Stage::Parse,
        "ready = 1",
    )?))?;

    cancelled.store(true, Ordering::Release);
    assert_cancelled(
        service.execute(&generate(
            LanguageProfile::Python(PythonVersion::Python314),
            Stage::LowerIr,
            "stop = 1",
        )?),
        b"stop = 1",
    )?;

    service.compiler.shutdown()?;
    fixture.remove()?;
    Ok(())
}

fn assert_generated(
    reply: ApplicationReply,
) -> Result<interface_core::GeneratedArtifact, LocalCompilerTestError> {
    let facts = match reply.outcome {
        ApplicationOutcome::Resolved(ReplyBody::Generated(facts))
            if ApplicationDisposition::from(&ReplyBody::Generated(facts))
                == ApplicationDisposition::Complete { emitted: 1 } =>
        {
            facts
        }
        observed => {
            return Err(LocalCompilerTestError::GeneratedOutcome {
                observed: Box::new(observed),
            });
        }
    };
    if facts.recipe.profile != LanguageProfile::Python(PythonVersion::Python314)
        || facts.recipe.stage != Stage::LowerIr
    {
        return Err(LocalCompilerTestError::GeneratedRecipe {
            profile: facts.recipe.profile,
            stage: facts.recipe.stage,
        });
    }
    if facts.source.byte_len != 9 {
        return Err(LocalCompilerTestError::GeneratedSourceLength {
            observed: facts.source.byte_len,
        });
    }
    if facts
        .publication
        .binding
        .as_ref()
        .iter()
        .all(|byte| *byte == 0)
    {
        return Err(LocalCompilerTestError::GeneratedBindingZero);
    }
    if facts.semantic_image.byte_len == 0
        || facts
            .semantic_image
            .identity
            .as_ref()
            .iter()
            .all(|byte| *byte == 0)
    {
        return Err(LocalCompilerTestError::GeneratedSemanticImageAbsent);
    }
    Ok(facts)
}

fn assert_missing_native_toolchain(reply: ApplicationReply) -> Result<(), LocalCompilerTestError> {
    match reply.outcome {
        ApplicationOutcome::Failed {
            diagnostic:
                Diagnostic {
                    code: DiagnosticCode::CompilerTerminal,
                    detail:
                        DiagnosticDetail::Compiler(CompilerTerminal::Toolchain {
                            language: Language::Rust,
                            stage: Stage::LowerIr,
                            selected: compiler_vocabulary::NativeTool::Rustc,
                            configured: None,
                            ..
                        }),
                },
        } => Ok(()),
        observed => Err(LocalCompilerTestError::CompilerDiagnostic {
            observed: Box::new(observed),
        }),
    }
}

fn assert_explicitly_unavailable_tool(
    reply: ApplicationReply,
) -> Result<(), LocalCompilerTestError> {
    match reply.outcome {
        ApplicationOutcome::Failed {
            diagnostic:
                Diagnostic {
                    code: DiagnosticCode::CompilerTerminal,
                    detail:
                        DiagnosticDetail::Compiler(CompilerTerminal::Toolchain {
                            language: Language::TypeScript,
                            stage: Stage::LowerIr,
                            selected: compiler_vocabulary::NativeTool::TypeScriptCompiler,
                            configured: None,
                            ..
                        }),
                },
        } => Ok(()),
        observed => Err(LocalCompilerTestError::CompilerDiagnostic {
            observed: Box::new(observed),
        }),
    }
}

fn assert_unsupported_stage(reply: ApplicationReply) -> Result<(), LocalCompilerTestError> {
    match reply.outcome {
        ApplicationOutcome::Failed {
            diagnostic:
                Diagnostic {
                    code: DiagnosticCode::UnsupportedCompilerStage,
                    ..
                },
        } => Ok(()),
        observed => Err(LocalCompilerTestError::CompilerDiagnostic {
            observed: Box::new(observed),
        }),
    }
}

fn assert_cancelled(reply: ApplicationReply, source: &[u8]) -> Result<(), LocalCompilerTestError> {
    let actual = source.len();
    let byte_len = u32::try_from(actual)
        .map_err(|source| LocalCompilerTestError::SourceLength { actual, source })?;
    let expected = ContentId::<SourceFactDomain>::from_canonical_bytes(source);
    match reply.outcome {
        ApplicationOutcome::Failed {
            diagnostic:
                Diagnostic {
                    code: DiagnosticCode::CompilerTerminal,
                    detail:
                        DiagnosticDetail::Compiler(CompilerTerminal::Cancelled {
                            attempted,
                            diagnostic: None,
                        }),
                },
        } if attempted.source.identity == expected && attempted.source.byte_len == byte_len => {
            Ok(())
        }
        observed => Err(LocalCompilerTestError::CompilerDiagnostic {
            observed: Box::new(observed),
        }),
    }
}
