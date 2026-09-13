//! Exercises the `compiler-driver` tests native-compile rejection contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    AuthorityFailure, CompileFailure, CompileOutput, CompileScratch, NativeTool,
    ToolchainSelection, compile,
};
use backend_semantic::vocabulary::{Language, Stage};

use super::support::*;

#[test]
fn oxc_rejection_retains_recipe_source_and_bounded_authority_diagnostic() -> Result<(), TestFailure>
{
    let language = Language::TypeScript;
    let tool = NativeTool::TypeScriptCompiler;
    let source = b"export const = ;";
    let source_length = source_length(source)?;
    let toolchain = direct_authority_toolchain(tool)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 128];
    let mut output = [0xa5; 512];
    match compile(
        request(
            language,
            source,
            ToolchainSelection::ResolvedNative(toolchain),
            &cancelled,
            Instant::now() + Duration::from_secs(1),
        ),
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: native_work.path(),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    ) {
        Err(CompileFailure::Authority {
            source_identity,
            recipe,
            failure:
                AuthorityFailure::TypeScript {
                    diagnostic: authority_diagnostic,
                    ..
                },
        }) => {
            if Language::from(recipe.profile) != language {
                return Err(TestFailure::RecipeLanguage {
                    tool,
                    expected: language,
                    actual: Language::from(recipe.profile),
                });
            }
            if source_identity.byte_len != source_length {
                return Err(TestFailure::FragmentSourceLength {
                    tool,
                    expected: source_length,
                    actual: source_identity.byte_len,
                });
            }
            if authority_diagnostic.primary.is_empty() {
                return Err(TestFailure::EmptyNativeDiagnostic { tool });
            }
            if authority_diagnostic.truncated {
                return Err(TestFailure::TruncatedNativeDiagnostic { tool });
            }
            if authority_diagnostic.observed != authority_diagnostic.primary.len() {
                return Err(TestFailure::NativeDiagnosticObserved {
                    tool,
                    expected: authority_diagnostic.primary.len(),
                    actual: authority_diagnostic.observed,
                });
            }
        }
        Err(failure) => {
            return Err(TestFailure::CompileTerminal {
                tool,
                expected: CompileExpectation::AuthorityInputRequired,
                observed: compile_terminal(&failure),
            });
        }
        Ok(_) => {
            return Err(TestFailure::CompileTerminal {
                tool,
                expected: CompileExpectation::AuthorityInputRequired,
                observed: CompileTerminal::Compiled,
            });
        }
    }
    native_work.assert_empty()?;
    if !output.iter().all(|byte| *byte == 0xa5) {
        return Err(TestFailure::OutputTailChanged { tool });
    }
    Ok(())
}

#[test]
fn native_lowering_rejects_an_unavailable_selection_before_any_tool_spawn()
-> Result<(), TestFailure> {
    let source = b"pub const alpha: bool = true;";
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 128];
    let mut output = [0xa5; 512];
    match compile(
        request(
            Language::Rust,
            source,
            ToolchainSelection::ExplicitlyUnavailable {
                tool: NativeTool::Rustc,
            },
            &cancelled,
            Instant::now() + Duration::from_secs(1),
        ),
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: native_work.path(),
        },
        CompileOutput {
            fragment_output: &mut output,
        },
    ) {
        Err(CompileFailure::ToolchainSelectionMismatch {
            language: Language::Rust,
            stage: Stage::LowerIr,
            selected: NativeTool::Rustc,
            ..
        }) => {}
        Err(failure) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Rustc,
                expected: CompileExpectation::CompactFact,
                observed: compile_terminal(&failure),
            });
        }
        Ok(_compiled) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Rustc,
                expected: CompileExpectation::CompactFact,
                observed: CompileTerminal::Compiled,
            });
        }
    }
    native_work.assert_empty()?;
    if !output.iter().all(|byte| *byte == 0xa5) {
        return Err(TestFailure::OutputTailChanged {
            tool: NativeTool::Rustc,
        });
    }
    Ok(())
}
