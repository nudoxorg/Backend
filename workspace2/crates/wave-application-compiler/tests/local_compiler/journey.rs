use std::sync::atomic::{AtomicBool, Ordering};

use nudox_compile_vocab::{Language, Stage};
use nudox_id::{ContentId, SourceFactDomain};
use wave_application_core::{
    ApplicationReply, ApplicationService, CompilerTerminal, Diagnostic, DiagnosticCode,
    DiagnosticDetail, ReplyBody, Terminal,
};

use super::support::{
    Fixture, LocalCompilerTestError, generate, open_local_compiler, rust_toolchains, rustc_path,
};

#[test]
fn configured_rust_compiler_lowers_publishes_and_preserves_exact_terminals()
-> Result<(), LocalCompilerTestError> {
    let fixture = Fixture::create()?;
    let cancelled = AtomicBool::new(false);
    let mut scratch = wave_application_compiler::LocalCompilerScratch::default();
    let rustc = rustc_path()?;
    let toolchains = rust_toolchains(&rustc)?;
    let compiler = open_local_compiler(&fixture, &toolchains, &cancelled, &mut scratch)?;
    let mut service = ApplicationService::with_compiler(compiler);

    assert_generated(&service.execute(&generate(
        Language::Rust,
        "lower-ir",
        "pub const READY: i32 = 1;",
    )?))?;
    assert_missing_native_toolchain(service.execute(&generate(
        Language::Python,
        "lower-ir",
        "ready = 1",
    )?))?;
    assert_explicitly_unavailable_tool(service.execute(&generate(
        Language::TypeScript,
        "lower-ir",
        "export const READY = 1;",
    )?))?;
    assert_unsupported_stage(service.execute(&generate(
        Language::Rust,
        "parse",
        "pub const READY: i32 = 1;",
    )?))?;

    cancelled.store(true, Ordering::Release);
    assert_cancelled(
        service.execute(&generate(
            Language::Rust,
            "lower-ir",
            "pub const STOP: i32 = 1;",
        )?),
        b"pub const STOP: i32 = 1;",
    )?;

    service.compiler.shutdown()?;
    fixture.remove()?;
    Ok(())
}

fn assert_generated(reply: &ApplicationReply) -> Result<(), LocalCompilerTestError> {
    let facts = match reply.body {
        ReplyBody::Generated(facts) => facts,
        observed => {
            return Err(LocalCompilerTestError::GeneratedBody {
                observed: Box::new(observed),
            });
        }
    };
    if reply.terminal != (Terminal::Complete { emitted: 1 }) {
        return Err(LocalCompilerTestError::GeneratedTerminal {
            observed: Box::new(reply.terminal),
        });
    }
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
    Ok(())
}

fn assert_missing_native_toolchain(reply: ApplicationReply) -> Result<(), LocalCompilerTestError> {
    match reply.diagnostic {
        Some(Diagnostic {
            code: DiagnosticCode::CompilerTerminal,
            detail:
                DiagnosticDetail::Compiler(CompilerTerminal::Toolchain {
                    language: Language::Python,
                    stage: Stage::LowerIr,
                    selected: nudox_compile_vocab::NativeTool::Python,
                    configured: None,
                    ..
                }),
        }) => Ok(()),
        observed => Err(LocalCompilerTestError::CompilerDiagnostic {
            observed: Box::new(observed),
        }),
    }
}

fn assert_explicitly_unavailable_tool(
    reply: ApplicationReply,
) -> Result<(), LocalCompilerTestError> {
    match reply.diagnostic {
        Some(Diagnostic {
            code: DiagnosticCode::CompilerTerminal,
            detail:
                DiagnosticDetail::Compiler(CompilerTerminal::Toolchain {
                    language: Language::TypeScript,
                    stage: Stage::LowerIr,
                    selected: nudox_compile_vocab::NativeTool::TypeScriptCompiler,
                    configured: None,
                    ..
                }),
        }) => Ok(()),
        observed => Err(LocalCompilerTestError::CompilerDiagnostic {
            observed: Box::new(observed),
        }),
    }
}

fn assert_unsupported_stage(reply: ApplicationReply) -> Result<(), LocalCompilerTestError> {
    match reply.diagnostic {
        Some(Diagnostic {
            code: DiagnosticCode::UnsupportedCompilerStage,
            ..
        }) => Ok(()),
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
    match reply.diagnostic {
        Some(Diagnostic {
            code: DiagnosticCode::CompilerTerminal,
            detail:
                DiagnosticDetail::Compiler(CompilerTerminal::Cancelled {
                    attempted,
                    diagnostic: None,
                }),
        }) if attempted.source.identity == expected && attempted.source.byte_len == byte_len => {
            Ok(())
        }
        observed => Err(LocalCompilerTestError::CompilerDiagnostic {
            observed: Box::new(observed),
        }),
    }
}
