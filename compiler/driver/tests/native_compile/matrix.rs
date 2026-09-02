//! Exercises the `compiler-driver` tests native-compile matrix contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{sync::atomic::AtomicBool, time::Instant};

use compiler_driver::{CompileOutput, CompileScratch, NativeTool, ToolchainSelection, compile};
use compiler_ir::{EntityKind, PrimitiveType, TypeNode};
use compiler_ir_vocabulary::{Confidence, ReferenceKind};
use compiler_vocabulary::{Language, Stage};

use super::support::*;

/// The clang matrix row must carry at least one decodable authority
/// occurrence: the reference plane is part of the compact-IR bar.
fn assert_clang_occurrence_plane(
    fragment: &compiler_ir::FragmentView<'_>,
) -> Result<(), TestFailure> {
    let Some(mut occurrences) = fragment.occurrences() else {
        return Err(TestFailure::OccurrencePlaneMissing {
            tool: NativeTool::Clang,
        });
    };
    let mut count = 0;
    while let Some(decoded) = occurrences.next() {
        match decoded {
            Ok(_) => count += 1,
            Err(cause) => {
                return Err(TestFailure::OccurrenceUndecodable {
                    tool: NativeTool::Clang,
                    cause,
                });
            }
        }
    }
    if count == 0 {
        return Err(TestFailure::OccurrencePlaneEmpty {
            tool: NativeTool::Clang,
        });
    }
    Ok(())
}

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
                if language == Language::Clang {
                    // The native authority must lend the rich reference and
                    // declared-type planes to the compact fragment, not just
                    // Constant/Record declaration rows.
                    assert_clang_occurrence_plane(&compiled.fragment)?;
                }
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

/// Proves the direct libclang authority lends its rich fact planes —
/// declaration rows, resolved occurrences with confidence tiers, and
/// authority-derived declared types — to the emitted compact fragment.
///
/// Every asserted coordinate is source-derived: mutating one reference,
/// recipe, or declaration span in the fixture changes the committed bytes.
#[test]
fn clang_authority_lends_occurrence_and_declared_type_planes_to_compact_ir()
-> Result<(), TestFailure> {
    let source = b"int target(int value) { return value; }\nint main(void) { return target(1); }\n";
    let executable = executable(NativeTool::Clang)?;
    let toolchain = resolved(NativeTool::Clang, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 4_096];
    let mut output = [0xa5; 4096];
    let compiled = compile(
        request(
            Language::Clang,
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
    )
    .map_err(|failure| TestFailure::CompileTerminal {
        tool: NativeTool::Clang,
        expected: CompileExpectation::CompactFact,
        observed: compile_terminal(&failure),
    })?;
    native_work.assert_empty()?;

    // Declaration plane: both callables and the declared parameter, in
    // traversal order.
    assert_facts(
        &compiled.fragment,
        &[
            (b"target", EntityKind::Function),
            (b"value", EntityKind::Parameter),
            (b"main", EntityKind::Function),
        ],
    )?;

    // Occurrence plane: three syntactic type uses, one oracle-resolved
    // parameter read, and one oracle-resolved call.
    let Some(mut occurrences) = compiled.fragment.occurrences() else {
        return Err(TestFailure::OccurrencePlaneMissing {
            tool: NativeTool::Clang,
        });
    };
    let mut type_references = 0;
    let mut variable_uses = 0;
    let mut function_calls = 0;
    let mut oracle = 0;
    let mut syntactic = 0;
    let mut call_relative_span = None;
    while let Some(decoded) = occurrences.next() {
        let decoded = match decoded {
            Ok(decoded) => decoded,
            Err(cause) => {
                return Err(TestFailure::OccurrenceUndecodable {
                    tool: NativeTool::Clang,
                    cause,
                });
            }
        };
        match decoded.occurrence.kind {
            ReferenceKind::TypeReference => type_references += 1,
            ReferenceKind::VariableUse => variable_uses += 1,
            ReferenceKind::FunctionCall => {
                function_calls += 1;
                call_relative_span = Some(decoded.occurrence.span);
            }
            ReferenceKind::MethodCall
            | ReferenceKind::MacroInvocation
            | ReferenceKind::FieldAccess
            | ReferenceKind::Import => {
                return Err(TestFailure::OccurrenceMix {
                    tool: NativeTool::Clang,
                });
            }
        }
        match decoded.occurrence.confidence {
            Confidence::Oracle => oracle += 1,
            Confidence::Syntactic | Confidence::Suffix | Confidence::Index | Confidence::Import => {
                syntactic += 1
            }
        }
    }
    if (type_references, variable_uses, function_calls) != (3, 1, 1) {
        return Err(TestFailure::OccurrenceMix {
            tool: NativeTool::Clang,
        });
    }
    if (oracle, syntactic) != (2, 3) {
        return Err(TestFailure::OccurrenceConfidence {
            tool: NativeTool::Clang,
        });
    }
    // The call use sits inside main's extent; its relative span starts at
    // the call spelling's offset from main's extent start.
    let Some(call_position) = source
        .windows(b"target".len())
        .rposition(|window| window == b"target")
    else {
        return Err(TestFailure::OccurrenceSpan {
            tool: NativeTool::Clang,
        });
    };
    let Some(main_extent_start) = source
        .windows(b"int main".len())
        .position(|window| window == b"int main")
    else {
        return Err(TestFailure::OccurrenceSpan {
            tool: NativeTool::Clang,
        });
    };
    let Some(call_span) = call_relative_span else {
        return Err(TestFailure::OccurrenceMix {
            tool: NativeTool::Clang,
        });
    };
    let expected_start = call_position as u32 - main_extent_start as u32;
    if call_span.start != expected_start || call_span.end != expected_start + b"target".len() as u32
    {
        return Err(TestFailure::OccurrenceSpan {
            tool: NativeTool::Clang,
        });
    }

    // Declared-type plane: every callable result and the parameter carry the
    // authority-derived integer recipe, so exactly one primitive type node
    // exists and every entity's type fact decodes against it.
    let primitive_types: Vec<PrimitiveType> = compiled
        .fragment
        .type_nodes()
        .filter_map(|node| match node {
            TypeNode::Primitive(primitive_type) => Some(primitive_type),
            _ => None,
        })
        .collect();
    if primitive_types.as_slice() != [PrimitiveType::I32] {
        return Err(TestFailure::PrimitiveType {
            tool: NativeTool::Clang,
            expected: PrimitiveType::I32,
            actual: primitive_types.first().copied(),
        });
    }
    let Some(mut type_facts) = compiled.fragment.type_facts() else {
        return Err(TestFailure::TypeFactsMissing {
            tool: NativeTool::Clang,
        });
    };
    let mut committed = 0;
    for _ in 0..3 {
        match type_facts.next() {
            Some(Ok(_)) => committed += 1,
            _ => {
                return Err(TestFailure::TypeFactsUndecodable {
                    tool: NativeTool::Clang,
                });
            }
        }
    }
    if committed != 3 {
        return Err(TestFailure::TypeFactsUndecodable {
            tool: NativeTool::Clang,
        });
    }
    Ok(())
}
