//! Exercises the `compiler-driver` tests native-compile matrix contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{sync::atomic::AtomicBool, time::Instant};

use compiler_driver::{CompileOutput, CompileScratch, NativeTool, ToolchainSelection, compile};
use compiler_ir::{EntityKind, PrimitiveType, TypeNode};
use compiler_vocabulary::{Language, Stage};

use super::support::*;

#[test]
fn native_adapters_parse_real_source_before_lending_compact_ir() -> Result<(), TestFailure> {
    #[allow(
        clippy::type_complexity,
        reason = "one homogeneous case table drives every closed language fact row"
    )]
    let cases: [(
        Language,
        NativeTool,
        &'static [u8],
        &'static [(&'static [u8], EntityKind)],
        PrimitiveType,
    ); 7] = [
        (
            Language::Rust,
            NativeTool::Rustc,
            b"pub const RUST_VALID: &str = \"yes\";".as_slice(),
            &[(b"RUST_VALID", EntityKind::Constant)],
            PrimitiveType::String,
        ),
        (
            Language::Python,
            NativeTool::Python,
            b"PYTHON_VALID = \"yes\"\n".as_slice(),
            &[(b"PYTHON_VALID", EntityKind::Constant)],
            PrimitiveType::String,
        ),
        (
            Language::Clang,
            NativeTool::Clang,
            b"const char *clang_valid = \"yes\";".as_slice(),
            &[(b"clang_valid", EntityKind::Constant)],
            PrimitiveType::String,
        ),
        (
            Language::TypeScript,
            NativeTool::TypeScriptCompiler,
            b"export const TYPESCRIPT_VALID: string = \"yes\";".as_slice(),
            &[(b"TYPESCRIPT_VALID", EntityKind::Constant)],
            PrimitiveType::String,
        ),
        (
            Language::CSharp,
            NativeTool::CSharpCompiler,
            b"public class Probe { public const string CSHARP_VALID = \"yes\"; }".as_slice(),
            &[
                (b"Probe", EntityKind::Record),
                (b"CSHARP_VALID", EntityKind::Constant),
            ],
            PrimitiveType::String,
        ),
        (
            Language::Go,
            NativeTool::GoCompiler,
            b"package fixture\nconst GO_VALID string = \"yes\"\n".as_slice(),
            &[
                (b"fixture", EntityKind::Module),
                (b"GO_VALID", EntityKind::Constant),
            ],
            PrimitiveType::String,
        ),
        (
            Language::Java,
            NativeTool::JavaCompiler,
            b"public final class JavaValid { public static final String NAME = \"yes\"; }"
                .as_slice(),
            &[
                (b"JavaValid", EntityKind::Record),
                (b"NAME", EntityKind::Constant),
            ],
            PrimitiveType::String,
        ),
    ];
    for (language, tool, source, expected_facts, expected_type) in cases {
        let source_length = source_length(source)?;
        let executable = executable(tool)?;
        let toolchain = resolved(tool, &executable)?;
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0xa5; 4_096];
        let mut output = [0xa5; 4096];
        let output_pointer = output.as_ptr();
        let fragment_len = match compile(
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
            Ok(compiled) => {
                if compiled.recipe.language != language {
                    return Err(TestFailure::RecipeLanguage {
                        tool,
                        expected: language,
                        actual: compiled.recipe.language,
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
                assert_facts(&compiled.fragment, expected_facts)?;
                assert_type_facts(&compiled.fragment, tool)?;
                // The only committed primitive is the spelled closed type;
                // every container fact stays type-opaque.
                let primitive_types: Vec<PrimitiveType> = compiled
                    .fragment
                    .type_nodes()
                    .filter_map(|node| match node {
                        TypeNode::Primitive(primitive_type) => Some(primitive_type),
                        _ => None,
                    })
                    .collect();
                if primitive_types.as_slice() != [expected_type] {
                    return Err(TestFailure::PrimitiveType {
                        tool,
                        expected: expected_type,
                        actual: primitive_types.first().copied(),
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
