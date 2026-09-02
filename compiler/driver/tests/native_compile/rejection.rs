//! Exercises the `compiler-driver` tests native-compile rejection contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileFailure, CompileOutput, CompileScratch, NativeTool, ToolchainSelection, compile,
};
use compiler_vocabulary::{Language, Stage};

use super::support::*;

#[test]
fn native_rejection_retains_recipe_source_and_bounded_diagnostic() -> Result<(), TestFailure> {
    let cases = [
        (
            Language::Rust,
            NativeTool::Rustc,
            b"pub const = ;".as_slice(),
        ),
        (
            Language::Clang,
            NativeTool::Clang,
            b"const char * = ;".as_slice(),
        ),
        (
            Language::TypeScript,
            NativeTool::TypeScriptCompiler,
            b"export const = ;".as_slice(),
        ),
        (
            Language::CSharp,
            NativeTool::CSharpCompiler,
            b"public class Probe { public const string = ; }".as_slice(),
        ),
        (
            Language::Go,
            NativeTool::GoCompiler,
            b"package fixture\nconst = ;\n".as_slice(),
        ),
        (
            Language::Java,
            NativeTool::JavaCompiler,
            b"public final class Broken { public static final String = ; }".as_slice(),
        ),
    ];
    for (language, tool, source) in cases {
        let source_length = source_length(source)?;
        let executable = executable(tool)?;
        let toolchain = resolved(tool, &executable)?;
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0; 4_096];
        let mut output = [0xa5; 512];
        match compile(
            request(
                language,
                source,
                ToolchainSelection::ResolvedNative(toolchain),
                &cancelled,
                Instant::now() + Duration::from_secs(5),
            ),
            CompileScratch {
                diagnostic_output: &mut diagnostic,
                native_work: native_work.path(),
            },
            CompileOutput {
                fragment_output: &mut output,
            },
        ) {
            Err(CompileFailure::NativeRejected {
                source_identity,
                recipe,
                diagnostic,
                ..
            }) => {
                if Language::from(recipe.profile) != language {
                    return Err(TestFailure::RecipeLanguage {
                        tool,
                        expected: language,
                        actual: Language::from(recipe.profile),
                    });
                }
                if recipe.stage != Stage::LowerIr {
                    return Err(TestFailure::RecipeStage {
                        tool,
                        expected: Stage::LowerIr,
                        actual: recipe.stage,
                    });
                }
                if recipe.tool != tool {
                    return Err(TestFailure::RecipeTool {
                        tool,
                        expected: tool,
                        actual: recipe.tool,
                    });
                }
                if source_identity.byte_len != source_length {
                    return Err(TestFailure::FragmentSourceLength {
                        tool,
                        expected: source_length,
                        actual: source_identity.byte_len,
                    });
                }
                if diagnostic.bytes.is_empty() {
                    return Err(TestFailure::EmptyNativeDiagnostic { tool });
                }
                if diagnostic.truncated {
                    return Err(TestFailure::TruncatedNativeDiagnostic { tool });
                }
                if diagnostic.observed != diagnostic.bytes.len() {
                    return Err(TestFailure::NativeDiagnosticObserved {
                        tool,
                        expected: diagnostic.bytes.len(),
                        actual: diagnostic.observed,
                    });
                }
            }
            Err(failure) => {
                return Err(TestFailure::CompileTerminal {
                    tool,
                    expected: CompileExpectation::NativeRejection,
                    observed: compile_terminal(&failure),
                });
            }
            Ok(_compiled) => {
                return Err(TestFailure::CompileTerminal {
                    tool,
                    expected: CompileExpectation::NativeRejection,
                    observed: CompileTerminal::Compiled,
                });
            }
        }
        native_work.assert_empty()?;
        if !output.iter().all(|byte| *byte == 0xa5) {
            return Err(TestFailure::OutputTailChanged { tool });
        }
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
