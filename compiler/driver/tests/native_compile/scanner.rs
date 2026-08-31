//! Exercises the `compiler-driver` tests native-compile scanner contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileFailure, CompileOutput, CompileScratch, LoweringUnsupported, NativeTool,
    ToolchainSelection, compile,
};
use compiler_ir::{EntityKind, PrimitiveType, TypeNode};
use compiler_vocabulary::Language;

use super::support::*;

#[test]
fn native_subset_lowering_ignores_comments_literals_and_nested_declarations()
-> Result<(), TestFailure> {
    let cases = [
        (
            Language::TypeScript,
            NativeTool::TypeScriptCompiler,
            b"// const COMMENT_ONLY: string = \"yes\";\n".as_slice(),
            LoweringUnsupported::TypeScriptDeclarationForm,
        ),
        (
            Language::TypeScript,
            NativeTool::TypeScriptCompiler,
            b"\"const LITERAL_ONLY: string = 'yes';\";\n".as_slice(),
            LoweringUnsupported::TypeScriptDeclarationForm,
        ),
        (
            Language::TypeScript,
            NativeTool::TypeScriptCompiler,
            b"(() => { const NESTED_ONLY: string = \"yes\"; })();\n".as_slice(),
            LoweringUnsupported::TypeScriptDeclarationForm,
        ),
        (
            Language::CSharp,
            NativeTool::CSharpCompiler,
            b"// const string COMMENT_ONLY = \"yes\";\n".as_slice(),
            LoweringUnsupported::CSharpDeclarationForm,
        ),
        (
            Language::CSharp,
            NativeTool::CSharpCompiler,
            b"public sealed class Probe { public string Text = \"const string LITERAL_ONLY = \\\"yes\\\";\"; }"
                .as_slice(),
            LoweringUnsupported::CSharpDeclarationForm,
        ),
        (
            Language::CSharp,
            NativeTool::CSharpCompiler,
            b"public sealed class Probe { public void Outer() { const string NESTED_ONLY = \"yes\"; } }"
                .as_slice(),
            LoweringUnsupported::CSharpDeclarationForm,
        ),
    ];
    for (language, tool, source, expected) in cases {
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
            Err(CompileFailure::LoweringUnsupported { cause, .. }) if cause == expected => {}
            Err(failure) => {
                return Err(TestFailure::CompileTerminal {
                    tool,
                    expected: CompileExpectation::LoweringUnsupported(expected),
                    observed: compile_terminal(&failure),
                });
            }
            Ok(_compiled) => {
                return Err(TestFailure::CompileTerminal {
                    tool,
                    expected: CompileExpectation::LoweringUnsupported(expected),
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
fn go_java_lowering_ignores_comments_literals_and_nested_declarations() -> Result<(), TestFailure> {
    let cases = [
        (
            Language::Go,
            NativeTool::GoCompiler,
            b"package fixture\nvar text = \"const LITERAL string = \\\"bad\\\"\"\nfunc GO_NESTED() bool { const INNER bool = true; return INNER }\n"
                .as_slice(),
            b"GO_NESTED".as_slice(),
            EntityKind::Function,
            PrimitiveType::Bool,
        ),
        (
            Language::Java,
            NativeTool::JavaCompiler,
            b"public final class JavaFalsifier { public static final String REAL = \"yes\"; public static boolean method() { String literal = \"String LITERAL = \\\"bad\\\"\"; return true; } }"
                .as_slice(),
            b"REAL".as_slice(),
            EntityKind::Constant,
            PrimitiveType::String,
        ),
    ];
    for (language, tool, source, expected_atom, expected_kind, expected_type) in cases {
        let executable = executable(tool)?;
        let toolchain = resolved(tool, &executable)?;
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0; 4_096];
        let mut output = [0xa5; 512];
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
        let actual_kind = compiled
            .fragment
            .entities()
            .next()
            .map(|entity| entity.kind);
        if actual_kind != Some(expected_kind) {
            return Err(TestFailure::EntityKind {
                tool,
                expected: expected_kind,
                actual: actual_kind,
            });
        }
        let atom = compiled.fragment.atoms().next().map(|atom| atom.bytes);
        if atom != Some(expected_atom) {
            return Err(TestFailure::AtomLength {
                tool,
                expected: expected_atom.len(),
                actual: atom.map(<[u8]>::len),
            });
        }
        let primitive_type = compiled
            .fragment
            .type_nodes()
            .next()
            .and_then(|node| match node {
                TypeNode::Primitive(primitive_type) => Some(primitive_type),
                _ => None,
            });
        if primitive_type != Some(expected_type) {
            return Err(TestFailure::PrimitiveType {
                tool,
                expected: expected_type,
                actual: primitive_type,
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
            LoweringUnsupported::NoSupportedDeclaration,
        ),
        (
            Language::Rust,
            NativeTool::Rustc,
            b"const TEXT: &str = \"pub const LITERAL_ONLY: bool = true;\";\n".as_slice(),
            LoweringUnsupported::NoSupportedDeclaration,
        ),
        (
            Language::Rust,
            NativeTool::Rustc,
            b"mod nested { pub const NESTED_ONLY: bool = true; }\n".as_slice(),
            LoweringUnsupported::NoSupportedDeclaration,
        ),
        (
            Language::Clang,
            NativeTool::Clang,
            b"// const char *COMMENT_ONLY = \"yes\";\n".as_slice(),
            LoweringUnsupported::NoSupportedDeclaration,
        ),
        (
            Language::Clang,
            NativeTool::Clang,
            b"char *text = \"const char *LITERAL_ONLY = \\\"yes\\\";\";\n".as_slice(),
            LoweringUnsupported::NoSupportedDeclaration,
        ),
        (
            Language::Clang,
            NativeTool::Clang,
            b"void outer(void) { const char *NESTED_ONLY = \"yes\"; }\n".as_slice(),
            LoweringUnsupported::NoSupportedDeclaration,
        ),
        (
            Language::Python,
            NativeTool::Python,
            b"# COMMENT_ONLY = \"yes\"\n".as_slice(),
            LoweringUnsupported::NoSupportedDeclaration,
        ),
        (
            Language::Python,
            NativeTool::Python,
            b"\"LITERAL_ONLY = True\"\n".as_slice(),
            LoweringUnsupported::NoSupportedDeclaration,
        ),
        (
            Language::Python,
            NativeTool::Python,
            b"def outer():\n    NESTED_ONLY = True\n".as_slice(),
            LoweringUnsupported::NoSupportedDeclaration,
        ),
    ];
    for (language, tool, source, expected) in cases {
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
            Err(CompileFailure::LoweringUnsupported { cause, .. }) if cause == expected => {}
            Err(failure) => {
                return Err(TestFailure::CompileTerminal {
                    tool,
                    expected: CompileExpectation::LoweringUnsupported(expected),
                    observed: compile_terminal(&failure),
                });
            }
            Ok(_compiled) => {
                return Err(TestFailure::CompileTerminal {
                    tool,
                    expected: CompileExpectation::LoweringUnsupported(expected),
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
