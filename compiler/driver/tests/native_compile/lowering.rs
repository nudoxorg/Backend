//! Exercises the `compiler-driver` tests native-compile lowering contract through its observable boundary.
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
fn native_subset_declaration_forms_produce_their_closed_compact_facts() -> Result<(), TestFailure> {
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
    ); 4] = [
        (
            Language::TypeScript,
            NativeTool::TypeScriptCompiler,
            b"export function TYPESCRIPT_FUNCTION(): boolean { return true; }".as_slice(),
            &[(b"TYPESCRIPT_FUNCTION", EntityKind::Function)],
            PrimitiveType::Bool,
        ),
        (
            Language::CSharp,
            NativeTool::CSharpCompiler,
            b"public class Probe { public static int CSHARP_FUNCTION() { return 1; } }".as_slice(),
            &[
                (b"Probe", EntityKind::Record),
                (b"CSHARP_FUNCTION", EntityKind::Function),
            ],
            PrimitiveType::I32,
        ),
        (
            Language::Go,
            NativeTool::GoCompiler,
            b"package fixture\nfunc GO_FUNCTION() bool { return true }\n".as_slice(),
            &[
                (b"fixture", EntityKind::Module),
                (b"GO_FUNCTION", EntityKind::Function),
            ],
            PrimitiveType::Bool,
        ),
        (
            Language::Java,
            NativeTool::JavaCompiler,
            b"public final class JavaFunction { public static int JAVA_FUNCTION() { return 1; } }"
                .as_slice(),
            &[
                (b"JavaFunction", EntityKind::Record),
                (b"JAVA_FUNCTION", EntityKind::Function),
            ],
            PrimitiveType::I32,
        ),
    ];
    for (language, tool, source, expected_facts, expected_type) in cases {
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
        assert_facts(&compiled.fragment, expected_facts)?;
        assert_type_facts(&compiled.fragment, tool)?;
        // Every container fact stays type-opaque; the only committed
        // primitive is the member's spelled closed type.
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
        native_work.assert_empty()?;
    }
    Ok(())
}

#[test]
fn unsupported_rust_outer_type_cannot_borrow_an_inner_bool_annotation() -> Result<(), TestFailure> {
    let source = b"pub const alpha: u64 = { const INNER: bool = true; 1 };";
    let executable = executable(NativeTool::Rustc)?;
    let toolchain = resolved(NativeTool::Rustc, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 4_096];
    let mut output = [0xa5; 4096];
    match compile(
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
        CompileOutput {
            fragment_output: &mut output,
        },
    ) {
        Err(CompileFailure::LoweringUnsupported {
            cause: LoweringUnsupported::NoSupportedDeclaration,
            ..
        }) => {}
        Err(failure) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Rustc,
                expected: CompileExpectation::LoweringUnsupported(
                    LoweringUnsupported::NoSupportedDeclaration,
                ),
                observed: compile_terminal(&failure),
            });
        }
        Ok(_compiled) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Rustc,
                expected: CompileExpectation::LoweringUnsupported(
                    LoweringUnsupported::NoSupportedDeclaration,
                ),
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

#[test]
fn one_more_than_the_fact_lane_capacity_returns_the_closed_terminal() -> Result<(), TestFailure> {
    let mut source = String::new();
    for ordinal in 0..=128 {
        source.push_str(&format!("pub static CAPACITY_{ordinal}: bool = true;\n"));
    }
    let executable = executable(NativeTool::Rustc)?;
    let toolchain = resolved(NativeTool::Rustc, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 4_096];
    let mut output = [0xa5; 4_096];
    match compile(
        request(
            Language::Rust,
            source.as_bytes(),
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
        Err(CompileFailure::LoweringUnsupported {
            cause: LoweringUnsupported::NoSupportedDeclaration,
            ..
        }) => {}
        Err(failure) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Rustc,
                expected: CompileExpectation::LoweringUnsupported(
                    LoweringUnsupported::NoSupportedDeclaration,
                ),
                observed: compile_terminal(&failure),
            });
        }
        Ok(_) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Rustc,
                expected: CompileExpectation::LoweringUnsupported(
                    LoweringUnsupported::NoSupportedDeclaration,
                ),
                observed: CompileTerminal::Compiled,
            });
        }
    }
    native_work.assert_empty()?;
    assert!(output.iter().all(|byte| *byte == 0xa5));
    Ok(())
}
#[test]
fn rust_declaration_atoms_kinds_and_types_reject_source_digest_only_lowering()
-> Result<(), TestFailure> {
    let alpha_source = b"pub const alpha: bool = true;";
    let bravo_source = b"pub const bravo: i32 = 1;";
    let executable = executable(NativeTool::Rustc)?;
    let toolchain = resolved(NativeTool::Rustc, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut alpha_diagnostic = [0; 4_096];
    let mut bravo_diagnostic = [0; 4_096];
    let mut alpha_output = [0; 4096];
    let mut bravo_output = [0; 4096];
    let alpha = compile(
        request(
            Language::Rust,
            alpha_source,
            ToolchainSelection::ResolvedNative(toolchain),
            &cancelled,
            Instant::now() + Duration::from_secs(5),
        ),
        CompileScratch {
            diagnostic_output: &mut alpha_diagnostic,
            native_work: native_work.path(),
        },
        CompileOutput {
            fragment_output: &mut alpha_output,
        },
    )
    .map_err(|failure| TestFailure::CompileTerminal {
        tool: NativeTool::Rustc,
        expected: CompileExpectation::CompactFact,
        observed: compile_terminal(&failure),
    })?;
    native_work.assert_empty()?;
    let bravo = compile(
        request(
            Language::Rust,
            bravo_source,
            ToolchainSelection::ResolvedNative(toolchain),
            &cancelled,
            Instant::now() + Duration::from_secs(5),
        ),
        CompileScratch {
            diagnostic_output: &mut bravo_diagnostic,
            native_work: native_work.path(),
        },
        CompileOutput {
            fragment_output: &mut bravo_output,
        },
    )
    .map_err(|failure| TestFailure::CompileTerminal {
        tool: NativeTool::Rustc,
        expected: CompileExpectation::CompactFact,
        observed: compile_terminal(&failure),
    })?;
    native_work.assert_empty()?;

    if alpha.fragment.as_ref() == bravo.fragment.as_ref() {
        return Err(TestFailure::ExpectedDistinctFact {
            fact: FragmentFact::Bytes,
        });
    }
    if alpha.source.identity == bravo.source.identity {
        return Err(TestFailure::ExpectedDistinctFact {
            fact: FragmentFact::SourceIdentity,
        });
    }
    if alpha.recipe.identity == bravo.recipe.identity {
        return Err(TestFailure::ExpectedDistinctFact {
            fact: FragmentFact::RecipeIdentity,
        });
    }
    let alpha_kind = alpha.fragment.entities().next().map(|entity| entity.kind);
    if alpha_kind != Some(EntityKind::Constant) {
        return Err(TestFailure::EntityKind {
            tool: NativeTool::Rustc,
            expected: EntityKind::Constant,
            actual: alpha_kind,
        });
    }
    let alpha_atom = alpha.fragment.atoms().next().map(|atom| atom.bytes);
    if alpha_atom != Some(&b"alpha"[..]) {
        return Err(TestFailure::AtomLength {
            tool: NativeTool::Rustc,
            expected: b"alpha".len(),
            actual: alpha_atom.map(<[u8]>::len),
        });
    }
    let bravo_atom = bravo.fragment.atoms().next().map(|atom| atom.bytes);
    if bravo_atom != Some(&b"bravo"[..]) {
        return Err(TestFailure::AtomLength {
            tool: NativeTool::Rustc,
            expected: b"bravo".len(),
            actual: bravo_atom.map(<[u8]>::len),
        });
    }
    let alpha_type = alpha
        .fragment
        .type_nodes()
        .next()
        .and_then(|node| match node {
            TypeNode::Primitive(primitive_type) => Some(primitive_type),
            _ => None,
        });
    if alpha_type != Some(PrimitiveType::Bool) {
        return Err(TestFailure::PrimitiveType {
            tool: NativeTool::Rustc,
            expected: PrimitiveType::Bool,
            actual: alpha_type,
        });
    }
    let bravo_type = bravo
        .fragment
        .type_nodes()
        .next()
        .and_then(|node| match node {
            TypeNode::Primitive(primitive_type) => Some(primitive_type),
            _ => None,
        });
    if bravo_type != Some(PrimitiveType::I32) {
        return Err(TestFailure::PrimitiveType {
            tool: NativeTool::Rustc,
            expected: PrimitiveType::I32,
            actual: bravo_type,
        });
    }
    Ok(())
}
