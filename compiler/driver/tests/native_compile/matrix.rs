//! Exercises the `compiler-driver` tests native-compile matrix contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileOutput, CompileScratch, NativeTool, ToolchainSelection, compile, compile_ir,
};
use compiler_ir::{PrimitiveType, SemanticImageAuthority, TypeNode};
use compiler_vocabulary::{Language, LanguageProfile, RustEdition, Stage};

use super::support::*;

#[test]
fn native_adapters_parse_real_source_before_lending_compact_ir() -> Result<(), TestFailure> {
    let cases = [
        (
            Language::Rust,
            NativeTool::Rustc,
            b"pub const RUST_VALID: &str = \"yes\";".as_slice(),
            b"RUST_VALID".as_slice(),
            PrimitiveType::String,
        ),
        (
            Language::Python,
            NativeTool::Python,
            b"PYTHON_VALID = \"yes\"\n".as_slice(),
            b"PYTHON_VALID".as_slice(),
            PrimitiveType::String,
        ),
        (
            Language::Clang,
            NativeTool::Clang,
            b"const char *clang_valid = \"yes\";".as_slice(),
            b"clang_valid".as_slice(),
            PrimitiveType::String,
        ),
        (
            Language::TypeScript,
            NativeTool::TypeScriptCompiler,
            b"export const TYPESCRIPT_VALID: string = \"yes\";".as_slice(),
            b"TYPESCRIPT_VALID".as_slice(),
            PrimitiveType::String,
        ),
        (
            Language::CSharp,
            NativeTool::CSharpCompiler,
            b"public class Probe { public const string CSHARP_VALID = \"yes\"; }".as_slice(),
            b"CSHARP_VALID".as_slice(),
            PrimitiveType::String,
        ),
        (
            Language::Go,
            NativeTool::GoCompiler,
            b"package fixture\nconst GO_VALID string = \"yes\"\n".as_slice(),
            b"GO_VALID".as_slice(),
            PrimitiveType::String,
        ),
        (
            Language::Java,
            NativeTool::JavaCompiler,
            b"public final class JavaValid { public static final String NAME = \"yes\"; }"
                .as_slice(),
            b"NAME".as_slice(),
            PrimitiveType::String,
        ),
    ];
    for (language, tool, source, expected_atom, expected_type) in cases {
        let source_length = source_length(source)?;
        let executable = executable(tool)?;
        let toolchain = resolved(tool, &executable)?;
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0xa5; 4_096];
        let mut output = [0xa5; 512];
        let output_pointer = output.as_ptr();
        let fragment_len = match compile(
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
            Ok(compiled) => {
                if Language::from(compiled.recipe.profile) != language {
                    return Err(TestFailure::RecipeLanguage {
                        tool,
                        expected: language,
                        actual: Language::from(compiled.recipe.profile),
                    });
                }
                if compiled.recipe.stage != Stage::LowerIr {
                    return Err(TestFailure::RecipeStage {
                        tool,
                        expected: Stage::LowerIr,
                        actual: compiled.recipe.stage,
                    });
                }
                if compiled.recipe.tool != tool {
                    return Err(TestFailure::RecipeTool {
                        tool,
                        expected: tool,
                        actual: compiled.recipe.tool,
                    });
                }
                if compiled.source.byte_len != source_length {
                    return Err(TestFailure::FragmentSourceLength {
                        tool,
                        expected: source_length,
                        actual: compiled.source.byte_len,
                    });
                }
                if !core::ptr::eq(compiled.fragment.as_ref().as_ptr(), output_pointer) {
                    return Err(TestFailure::FragmentBorrow { tool });
                }
                let entity_count = compiled.fragment.entities().count();
                if entity_count != 1 {
                    return Err(TestFailure::EntityCount {
                        tool,
                        expected: 1,
                        actual: entity_count,
                    });
                }
                let type_node_count = compiled.fragment.type_nodes().count();
                if type_node_count != 1 {
                    return Err(TestFailure::TypeNodeCount {
                        tool,
                        expected: 1,
                        actual: type_node_count,
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
                let primitive_type =
                    compiled
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
                compiled.fragment.as_ref().len()
            }
            Err(failure) => {
                return Err(TestFailure::CompileTerminal {
                    tool,
                    expected: CompileExpectation::CompactFact,
                    observed: compile_terminal(&failure),
                });
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
fn semantic_ir_binds_the_exact_source_profile() -> Result<(), TestFailure> {
    let profile = LanguageProfile::Rust(RustEdition::Rust2024);
    let source = b"pub const PROFILE_BOUND: bool = true;";
    let tool = NativeTool::Rustc;
    let executable = executable(tool)?;
    let toolchain = resolved(tool, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 4_096];
    let compiled = compile_ir(
        request(
            Language::Rust,
            source,
            ToolchainSelection::ResolvedNative(toolchain),
            &cancelled,
            Instant::now() + Duration::from_secs(5),
        ),
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: native_work.path(),
        },
    )
    .map_err(|failure| TestFailure::CompileTerminal {
        tool,
        expected: CompileExpectation::CompactFact,
        observed: compile_terminal(&failure),
    })?;
    let actual = compiled.ir.storage_columns().authority;
    if actual != SemanticImageAuthority::Language(profile) {
        return Err(TestFailure::SemanticAuthority {
            expected: profile,
            actual,
        });
    }
    native_work.assert_empty()
}
