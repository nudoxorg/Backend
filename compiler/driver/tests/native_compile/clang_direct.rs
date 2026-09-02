//! Exercises the `compiler-driver` direct Clang frontend journey through its observable boundary.
//! The public compile terminal must run real semantic admission over a real C fixture: linked
//! builds compile and reject typed, unlinked builds fail with the typed unavailable cause, and
//! the declared executable is never spawned.
#[cfg(clang_native)]
use std::path::Path;
use std::{sync::atomic::AtomicBool, time::Instant};

#[cfg(clang_native)]
use compiler_driver::{ClangDiagnosticSeverity, ResolvedToolchain};
use compiler_driver::{
    ClangFailure, CompileFailure, CompileOutput, CompileScratch, NativeTool, ToolchainSelection,
    compile,
};
use compiler_vocabulary::Language;
#[cfg(clang_native)]
use compiler_vocabulary::Stage;

use super::support::*;

const VALID_SOURCE: &[u8] = b"const char *clang_direct = \"ok\";\n";

#[test]
fn clang_frontend_compiles_real_c_with_mode_exact_authority() -> Result<(), TestFailure> {
    let expected_length = source_length(VALID_SOURCE)?;
    let executable = executable(NativeTool::Clang)?;
    let toolchain = resolved(NativeTool::Clang, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 4_096];
    let mut output = [0xa5; 512];
    let output_pointer = output.as_ptr();
    match compile(
        request(
            Language::Clang,
            VALID_SOURCE,
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
            if !cfg!(clang_native) {
                return Err(TestFailure::CompiledWithoutAuthority {
                    tool: NativeTool::Clang,
                });
            }
            if compiled.source.byte_len != expected_length {
                return Err(TestFailure::FragmentSourceLength {
                    tool: NativeTool::Clang,
                    expected: expected_length,
                    actual: compiled.source.byte_len,
                });
            }
            if !core::ptr::eq(compiled.fragment.as_ref().as_ptr(), output_pointer) {
                return Err(TestFailure::FragmentBorrow {
                    tool: NativeTool::Clang,
                });
            }
            let atom = compiled.fragment.atoms().next().map(|atom| atom.bytes);
            if atom != Some(b"clang_direct".as_slice()) {
                return Err(TestFailure::AtomLength {
                    tool: NativeTool::Clang,
                    expected: b"clang_direct".len(),
                    actual: atom.map(<[u8]>::len),
                });
            }
            let entity_count = compiled.fragment.entities().count();
            if entity_count != 1 {
                return Err(TestFailure::EntityCount {
                    tool: NativeTool::Clang,
                    expected: 1,
                    actual: entity_count,
                });
            }
            let type_node_count = compiled.fragment.type_nodes().count();
            if type_node_count != 1 {
                return Err(TestFailure::TypeNodeCount {
                    tool: NativeTool::Clang,
                    expected: 1,
                    actual: type_node_count,
                });
            }
        }
        Err(CompileFailure::ClangFrontend {
            source_identity,
            recipe,
            cause: ClangFailure::LibclangUnavailable,
        }) if !cfg!(clang_native) => {
            let _ = (source_identity, recipe);
        }
        Err(failure) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Clang,
                expected: CompileExpectation::CompactFact,
                observed: compile_terminal(&failure),
            });
        }
    }
    native_work.assert_empty()?;
    Ok(())
}

#[cfg(clang_native)]
#[test]
fn malformed_c_is_rejected_typed_without_a_fragment() -> Result<(), TestFailure> {
    let source = b"int broken( {\n";
    let expected_length = source_length(source)?;
    let executable = executable(NativeTool::Clang)?;
    let toolchain = resolved(NativeTool::Clang, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 4_096];
    let mut output = [0xa5; 512];
    match compile(
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
    ) {
        Err(CompileFailure::ClangFrontend {
            source_identity,
            recipe,
            cause:
                ClangFailure::ParseRejected {
                    source_name,
                    diagnostic: Some(rejection),
                },
        }) => {
            if rejection.severity != ClangDiagnosticSeverity::Error {
                return Err(TestFailure::ClangRejectionSeverity {
                    tool: NativeTool::Clang,
                });
            }
            if rejection.line != 1 || rejection.column == 0 {
                return Err(TestFailure::ClangRejectionCoordinate {
                    tool: NativeTool::Clang,
                });
            }
            if source_identity.byte_len != expected_length {
                return Err(TestFailure::FragmentSourceLength {
                    tool: NativeTool::Clang,
                    expected: expected_length,
                    actual: source_identity.byte_len,
                });
            }
            if recipe.language != Language::Clang || recipe.tool != NativeTool::Clang {
                return Err(TestFailure::RecipeLanguage {
                    tool: NativeTool::Clang,
                    expected: Language::Clang,
                    actual: recipe.language,
                });
            }
            if recipe.stage != Stage::LowerIr {
                return Err(TestFailure::RecipeStage {
                    tool: NativeTool::Clang,
                    expected: Stage::LowerIr,
                    actual: recipe.stage,
                });
            }
            assert_eq!(source_name, Path::new("source.c"));
        }
        Err(failure) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Clang,
                expected: CompileExpectation::NativeRejection,
                observed: compile_terminal(&failure),
            });
        }
        Ok(_compiled) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Clang,
                expected: CompileExpectation::NativeRejection,
                observed: CompileTerminal::Compiled,
            });
        }
    }
    if !output.iter().all(|byte| *byte == 0xa5) {
        return Err(TestFailure::OutputTailChanged {
            tool: NativeTool::Clang,
        });
    }
    native_work.assert_empty()?;
    Ok(())
}

#[cfg(clang_native)]
#[test]
fn the_direct_frontend_never_spawns_its_declared_executable() -> Result<(), TestFailure> {
    let source = b"const char *unspawned = \"ok\";\n";
    let missing = Path::new("/nonexistent/compiler-driver-direct-clang");
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::Clang,
        missing,
        b"declared direct authority identity",
    )?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 4_096];
    let mut output = [0xa5; 512];
    match compile(
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
    ) {
        Ok(compiled) => {
            let atom = compiled.fragment.atoms().next().map(|atom| atom.bytes);
            if atom != Some(b"unspawned".as_slice()) {
                return Err(TestFailure::AtomLength {
                    tool: NativeTool::Clang,
                    expected: b"unspawned".len(),
                    actual: atom.map(<[u8]>::len),
                });
            }
        }
        Err(failure) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Clang,
                expected: CompileExpectation::CompactFact,
                observed: compile_terminal(&failure),
            });
        }
    }
    native_work.assert_empty()?;
    Ok(())
}
