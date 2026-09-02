//! Exercises the `compiler-driver` tests native-compile scanner contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    ClangFailure, CompileFailure, CompileOutput, CompileScratch, LoweringUnsupported, NativeTool,
    ToolchainSelection, compile,
};
use compiler_ir::{EntityKind, PrimitiveType, TypeNode};
use compiler_vocabulary::Language;

use super::support::*;

/// One pinned lowering outcome: either the closed no-declaration terminal
/// fires and no fragment byte is written, or the source provably lowers
/// exactly this entity sequence.
enum Expectation {
    Terminal(LoweringUnsupported),
    Facts(&'static [(&'static [u8], EntityKind)]),
}

#[test]
fn native_subset_lowering_ignores_comments_literals_and_nested_declarations()
-> Result<(), TestFailure> {
    let cases = [
        (
            Language::TypeScript,
            NativeTool::TypeScriptCompiler,
            b"// const COMMENT_ONLY: string = \"yes\";\n".as_slice(),
            Expectation::Terminal(LoweringUnsupported::NoSupportedDeclaration),
        ),
        (
            Language::TypeScript,
            NativeTool::TypeScriptCompiler,
            b"\"const LITERAL_ONLY: string = 'yes';\";\n".as_slice(),
            Expectation::Terminal(LoweringUnsupported::NoSupportedDeclaration),
        ),
        (
            Language::TypeScript,
            NativeTool::TypeScriptCompiler,
            b"(() => { const NESTED_ONLY: string = \"yes\"; })();\n".as_slice(),
            Expectation::Terminal(LoweringUnsupported::NoSupportedDeclaration),
        ),
        (
            Language::CSharp,
            NativeTool::CSharpCompiler,
            b"// const string COMMENT_ONLY = \"yes\";\n".as_slice(),
            Expectation::Terminal(LoweringUnsupported::NoSupportedDeclaration),
        ),
        (
            Language::CSharp,
            NativeTool::CSharpCompiler,
            b"public sealed class Probe { public string Text = \"const string LITERAL_ONLY = \\\"yes\\\";\"; }"
                .as_slice(),
            Expectation::Facts(&[(b"Probe", EntityKind::Record), (b"Text", EntityKind::Field)]),
        ),
        (
            Language::CSharp,
            NativeTool::CSharpCompiler,
            b"public sealed class Probe { public void Outer() { const string NESTED_ONLY = \"yes\"; } }"
                .as_slice(),
            Expectation::Facts(&[(b"Probe", EntityKind::Record), (b"Outer", EntityKind::Function)]),
        ),
    ];
    for (language, tool, source, expected) in cases {
        let executable = executable(tool)?;
        let toolchain = resolved(tool, &executable)?;
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0; 4_096];
        let mut output = [0xa5; 4096];
        let fragment_len = match expected {
            Expectation::Terminal(expected_cause) => {
                match compile(
                    request(
                        language,
                        source,
                        ToolchainSelection::ResolvedNative(toolchain),
                        &cancelled,
                        Instant::now() + Duration::from_secs(10),
                    ),
                    CompileScratch {
                        diagnostic_output: &mut diagnostic,
                        native_work: native_work.path(),
                    },
                    CompileOutput {
                        fragment_output: &mut output,
                    },
                ) {
                    Err(CompileFailure::LoweringUnsupported { cause, .. })
                        if cause == expected_cause =>
                    {
                        0
                    }
                    Err(failure) => {
                        return Err(TestFailure::CompileTerminal {
                            tool,
                            expected: CompileExpectation::LoweringUnsupported(expected_cause),
                            observed: compile_terminal(&failure),
                        });
                    }
                    Ok(_compiled) => {
                        return Err(TestFailure::CompileTerminal {
                            tool,
                            expected: CompileExpectation::LoweringUnsupported(expected_cause),
                            observed: CompileTerminal::Compiled,
                        });
                    }
                }
            }
            Expectation::Facts(expected_facts) => {
                let compiled = compile(
                    request(
                        language,
                        source,
                        ToolchainSelection::ResolvedNative(toolchain),
                        &cancelled,
                        Instant::now() + Duration::from_secs(10),
                    ),
                    CompileScratch {
                        diagnostic_output: &mut diagnostic,
                        native_work: native_work.path(),
                    },
                    CompileOutput {
                        fragment_output: &mut output,
                    },
                )
                .map_err(|failure| TestFailure::CompileTerminal {
                    tool,
                    expected: CompileExpectation::CompactFact,
                    observed: compile_terminal(&failure),
                })?;
                assert_facts(&compiled.fragment, expected_facts)?;
                assert_type_facts(&compiled.fragment, tool)?;
                compiled.fragment.as_ref().len()
            }
        };
        native_work.assert_empty()?;
        if !output[fragment_len..].iter().all(|byte| *byte == 0xa5) {
            return Err(TestFailure::OutputTailChanged { tool });
        }
    }
    Ok(())
}
#[test]
fn go_java_lowering_ignores_comments_literals_and_nested_declarations() -> Result<(), TestFailure> {
    #[allow(clippy::type_complexity, reason = "one homogeneous case table drives every closed language fact row")]
    let cases: [(Language, NativeTool, &'static [u8], &'static [(&'static [u8], EntityKind)], PrimitiveType); 2] = [
        (
            Language::Go,
            NativeTool::GoCompiler,
            b"package fixture\nvar text = \"const LITERAL string = \\\"bad\\\"\"\nfunc GO_NESTED() bool { const INNER bool = true; return INNER }\n"
                .as_slice(),
            &[(b"fixture", EntityKind::Module), (b"text", EntityKind::Static), (b"GO_NESTED", EntityKind::Function)],
            PrimitiveType::Bool,
        ),
        (
            Language::Java,
            NativeTool::JavaCompiler,
            b"public final class JavaFalsifier { public static final String REAL = \"yes\"; public static boolean method() { String literal = \"String LITERAL = \\\"bad\\\"\"; return true; } }"
                .as_slice(),
            &[(b"JavaFalsifier", EntityKind::Record), (b"REAL", EntityKind::Constant), (b"method", EntityKind::Function)],
            PrimitiveType::String,
        ),
    ];
    for (language, tool, source, expected, expected_type) in cases {
        let executable = executable(tool)?;
        let toolchain = resolved(tool, &executable)?;
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0; 4_096];
        let mut output = [0xa5; 4096];
        let compiled = compile(
            request(
                language,
                source,
                ToolchainSelection::ResolvedNative(toolchain),
                &cancelled,
                Instant::now() + Duration::from_secs(10),
            ),
            CompileScratch {
                diagnostic_output: &mut diagnostic,
                native_work: native_work.path(),
            },
            CompileOutput {
                fragment_output: &mut output,
            },
        )
        .map_err(|failure| TestFailure::CompileTerminal {
            tool,
            expected: CompileExpectation::CompactFact,
            observed: compile_terminal(&failure),
        })?;
        assert_facts(&compiled.fragment, expected)?;
        assert_type_facts(&compiled.fragment, tool)?;
        // The literal-only source bytes never became an inner declaration:
        // every committed primitive is still bounded by the spelled types.
        let primitives: Vec<PrimitiveType> = compiled
            .fragment
            .type_nodes()
            .filter_map(|node| match node {
                TypeNode::Primitive(primitive_type) => Some(primitive_type),
                _ => None,
            })
            .collect();
        if !primitives.contains(&expected_type) {
            return Err(TestFailure::PrimitiveType {
                tool,
                expected: expected_type,
                actual: primitives.first().copied(),
            });
        }
        native_work.assert_empty()?;
    }
    Ok(())
}
#[test]
fn existing_lowering_ignores_comments_literals_and_nested_declarations() -> Result<(), TestFailure>
{
    let cases = [
        (
            Language::Rust,
            NativeTool::Rustc,
            b"// pub const COMMENT_ONLY: bool = true;\n".as_slice(),
            Expectation::Terminal(LoweringUnsupported::NoSupportedDeclaration),
        ),
        (
            Language::Rust,
            NativeTool::Rustc,
            b"const TEXT: &str = \"pub const LITERAL_ONLY: bool = true;\";\n".as_slice(),
            Expectation::Facts(&[(b"TEXT", EntityKind::Constant)]),
        ),
        (
            Language::Rust,
            NativeTool::Rustc,
            b"mod nested { pub const NESTED_ONLY: bool = true; }\n".as_slice(),
            Expectation::Facts(&[(b"nested", EntityKind::Module)]),
        ),
        (
            Language::Clang,
            NativeTool::Clang,
            b"// const char *COMMENT_ONLY = \"yes\";\n".as_slice(),
            Expectation::Terminal(LoweringUnsupported::NoSupportedDeclaration),
        ),
        (
            Language::Clang,
            NativeTool::Clang,
            b"char *text = \"const char *LITERAL_ONLY = \\\"yes\\\";\";\n".as_slice(),
            Expectation::Terminal(LoweringUnsupported::NoSupportedDeclaration),
        ),
        (
            Language::Clang,
            NativeTool::Clang,
            b"void outer(void) { const char *NESTED_ONLY = \"yes\"; }\n".as_slice(),
            Expectation::Facts(&[(b"outer", EntityKind::Function)]),
        ),
        (
            Language::Python,
            NativeTool::Python,
            b"# COMMENT_ONLY = \"yes\"\n".as_slice(),
            Expectation::Terminal(LoweringUnsupported::NoSupportedDeclaration),
        ),
        (
            Language::Python,
            NativeTool::Python,
            b"\"LITERAL_ONLY = True\"\n".as_slice(),
            Expectation::Terminal(LoweringUnsupported::NoSupportedDeclaration),
        ),
        (
            Language::Python,
            NativeTool::Python,
            b"def outer():\n    NESTED_ONLY = True\n".as_slice(),
            Expectation::Facts(&[(b"outer", EntityKind::Function)]),
        ),
    ];
    for (language, tool, source, expected) in cases {
        let executable = executable(tool)?;
        let toolchain = resolved(tool, &executable)?;
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0; 4_096];
        let mut output = [0xa5; 4096];
        let fragment_len = match expected {
            Expectation::Terminal(expected_cause) => {
                match compile(
                    request(
                        language,
                        source,
                        ToolchainSelection::ResolvedNative(toolchain),
                        &cancelled,
                        Instant::now() + NATIVE_COMPILE_DEADLINE,
                    ),
                    CompileScratch {
                        diagnostic_output: &mut diagnostic,
                        native_work: native_work.path(),
                    },
                    CompileOutput {
                        fragment_output: &mut output,
                    },
                ) {
                    Err(CompileFailure::LoweringUnsupported { cause, .. })
                        if cause == expected_cause =>
                    {
                        0
                    }
                    Err(failure) => {
                        return Err(TestFailure::CompileTerminal {
                            tool,
                            expected: CompileExpectation::LoweringUnsupported(expected_cause),
                            observed: compile_terminal(&failure),
                        });
                    }
                    Ok(_compiled) => {
                        return Err(TestFailure::CompileTerminal {
                            tool,
                            expected: CompileExpectation::LoweringUnsupported(expected_cause),
                            observed: CompileTerminal::Compiled,
                        });
                    }
                }
            }
            Expectation::Facts(expected_facts) => {
                let compiled = match compile(
                    request(
                        language,
                        source,
                        ToolchainSelection::ResolvedNative(toolchain),
                        &cancelled,
                        Instant::now() + NATIVE_COMPILE_DEADLINE,
                    ),
                    CompileScratch {
                        diagnostic_output: &mut diagnostic,
                        native_work: native_work.path(),
                    },
                    CompileOutput {
                        fragment_output: &mut output,
                    },
                ) {
                    Ok(compiled) => compiled,
                    Err(CompileFailure::ClangFrontend {
                        cause: ClangFailure::LibclangUnavailable,
                        ..
                    }) if language == Language::Clang && !cfg!(clang_native) => {
                        native_work.assert_empty()?;
                        if !output.iter().all(|byte| *byte == 0xa5) {
                            return Err(TestFailure::OutputTailChanged { tool });
                        }
                        continue;
                    }
                    Err(failure) => {
                        return Err(TestFailure::CompileTerminal {
                            tool,
                            expected: CompileExpectation::CompactFact,
                            observed: compile_terminal(&failure),
                        });
                    }
                };
                assert_facts(&compiled.fragment, expected_facts)?;
                assert_type_facts(&compiled.fragment, tool)?;
                compiled.fragment.as_ref().len()
            }
        };
        native_work.assert_empty()?;
        if !output[fragment_len..].iter().all(|byte| *byte == 0xa5) {
            return Err(TestFailure::OutputTailChanged { tool });
        }
    }
    Ok(())
}
