//! Exercises the `compiler-application` tests local-compiler journey contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::sync::atomic::{AtomicBool, Ordering};

use compiler_vocabulary::{Language, Stage};
use heart_identity::{ContentId, SourceFactDomain};
use interface_core::{
    ApplicationDisposition, ApplicationOutcome, ApplicationReply, ApplicationService,
    CompilerTerminal, Diagnostic, DiagnosticCode, DiagnosticDetail, ReplyBody,
};

use super::support::{
    Fixture, LocalCompilerTestError, generate, open_local_compiler, rust_toolchains, rustc_path,
};

#[test]
fn configured_rust_compiler_lowers_publishes_and_preserves_exact_terminals()
-> Result<(), LocalCompilerTestError> {
    let fixture = Fixture::create()?;
    let cancelled = AtomicBool::new(false);
    let mut scratch = compiler_application::LocalCompilerScratch::default();
    let rustc = rustc_path()?;
    let toolchains = rust_toolchains(&rustc)?;
    let compiler = open_local_compiler(&fixture, &toolchains, &cancelled, &mut scratch)?;
    let mut service = ApplicationService::with_compiler(compiler);

    let first = assert_generated(service.execute(&generate(
        Language::Rust,
        Stage::LowerIr,
        "pub const READY: i32 = 1;",
    )?))?;
    let second = assert_generated(service.execute(&generate(
        Language::Rust,
        Stage::LowerIr,
        "pub const READY: i32 = 2;",
    )?))?;
    if first.source.identity == second.source.identity {
        return Err(LocalCompilerTestError::SourceIdentityCollision {
            first: first.source.identity,
            second: second.source.identity,
        });
    }
    assert_missing_native_toolchain(service.execute(&generate(
        Language::Python,
        Stage::LowerIr,
        "ready = 1",
    )?))?;
    assert_explicitly_unavailable_tool(service.execute(&generate(
        Language::TypeScript,
        Stage::LowerIr,
        "export const READY = 1;",
    )?))?;
    assert_unsupported_stage(service.execute(&generate(
        Language::Rust,
        Stage::Parse,
        "pub const READY: i32 = 1;",
    )?))?;

    cancelled.store(true, Ordering::Release);
    assert_cancelled(
        service.execute(&generate(
            Language::Rust,
            Stage::LowerIr,
            "pub const STOP: i32 = 1;",
        )?),
        b"pub const STOP: i32 = 1;",
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
    if facts.recipe.language != Language::Rust || facts.recipe.stage != Stage::LowerIr {
        return Err(LocalCompilerTestError::GeneratedRecipe {
            language: facts.recipe.language,
            stage: facts.recipe.stage,
        });
    }
    if facts.source.byte_len != 25 {
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
                            language: Language::Python,
                            stage: Stage::LowerIr,
                            selected: compiler_vocabulary::NativeTool::Python,
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
    let Ok(byte_len) = u32::try_from(source.len()) else {
        return Err(LocalCompilerTestError::CancelledSourceLength {
            actual: source.len(),
        });
    };
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
