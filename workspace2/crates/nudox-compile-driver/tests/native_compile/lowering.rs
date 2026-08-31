use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use nudox_compile_driver::{
    CompileFailure, CompileOutput, CompileScratch, LoweringUnsupported, NativeTool,
    ToolchainSelection, compile,
};
use nudox_compile_vocab::Language;
use nudox_ir_format::{EntityKind, PrimitiveType, TypeNode};

use super::support::*;

#[test]
fn native_subset_declaration_forms_produce_their_closed_compact_facts() -> Result<(), TestFailure> {
    let cases = [
        (
            Language::TypeScript,
            NativeTool::TypeScriptCompiler,
            b"export function TYPESCRIPT_FUNCTION(): boolean { return true; }".as_slice(),
            b"TYPESCRIPT_FUNCTION".as_slice(),
            EntityKind::Function,
            PrimitiveType::Bool,
        ),
        (
            Language::CSharp,
            NativeTool::CSharpCompiler,
            b"public class Probe { public static int CSHARP_FUNCTION() { return 1; } }".as_slice(),
            b"CSHARP_FUNCTION".as_slice(),
            EntityKind::Function,
            PrimitiveType::I32,
        ),
        (
            Language::Go,
            NativeTool::GoCompiler,
            b"package fixture\nfunc GO_FUNCTION() bool { return true }\n".as_slice(),
            b"GO_FUNCTION".as_slice(),
            EntityKind::Function,
            PrimitiveType::Bool,
        ),
        (
            Language::Java,
            NativeTool::JavaCompiler,
            b"public final class JavaFunction { public static int JAVA_FUNCTION() { return 1; } }"
                .as_slice(),
            b"JAVA_FUNCTION".as_slice(),
            EntityKind::Function,
            PrimitiveType::I32,
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
fn unsupported_rust_outer_type_cannot_borrow_an_inner_bool_annotation() -> Result<(), TestFailure> {
    let source = b"pub const alpha: u64 = { const INNER: bool = true; 1 };";
    let executable = executable(NativeTool::Rustc)?;
    let toolchain = resolved(NativeTool::Rustc, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0; 4_096];
    let mut output = [0xa5; 512];
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
            cause: LoweringUnsupported::RustConstantType,
            ..
        }) => {}
        Err(failure) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Rustc,
                expected: CompileExpectation::LoweringUnsupported(
                    LoweringUnsupported::RustConstantType,
                ),
                observed: compile_terminal(&failure),
            });
        }
        Ok(_compiled) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Rustc,
                expected: CompileExpectation::LoweringUnsupported(
                    LoweringUnsupported::RustConstantType,
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
    let mut alpha_output = [0; 512];
    let mut bravo_output = [0; 512];
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
