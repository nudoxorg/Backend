//! Exercises direct in-process frontend matrix behavior through public driver requests.
//! Covers source identity, canonical profile binding, and caller-owned fragment borrowing.
//! Leaves project-bearing profile successes to their real authority-image and HIR fixtures.

use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileOutput, CompileScratch, NativeTool, ToolchainSelection, compile, compile_ir,
};
use compiler_ir::{EntityKind, SemanticImageAuthority};
use backend_semantic::vocabulary::{Language, LanguageProfile, PythonVersion};

use super::support::*;

#[test]
fn direct_in_process_frontends_bind_real_source_without_native_work() -> Result<(), TestFailure> {
    let cases = [
        (
            Language::Python,
            NativeTool::Python,
            b"PYTHON_VALID = 'yes'\n".as_slice(),
            &[(b"PYTHON_VALID".as_slice(), EntityKind::Static)][..],
        ),
        (
            Language::Clang,
            NativeTool::Clang,
            b"const char *clang_valid = \"yes\";".as_slice(),
            &[(b"clang_valid".as_slice(), EntityKind::Static)][..],
        ),
        (
            Language::TypeScript,
            NativeTool::TypeScriptCompiler,
            b"export const TYPESCRIPT_VALID = 'yes';".as_slice(),
            &[(b"TYPESCRIPT_VALID".as_slice(), EntityKind::Constant)][..],
        ),
    ];
    for (language, tool, source, expected) in cases {
        let executable = match language {
            Language::Python | Language::Clang => Some(executable(tool)?),
            Language::TypeScript => None,
            Language::Rust | Language::Go | Language::Java | Language::CSharp => {
                return Err(TestFailure::CompileTerminal {
                    tool,
                    expected: CompileExpectation::CompactFact,
                    observed: CompileTerminal::AuthorityInputRequired,
                });
            }
        };
        let resolved = match executable.as_deref() {
            Some(executable) => resolved(tool, executable)?,
            None => direct_authority_toolchain(tool)?,
        };
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0xa5; 4_096];
        let mut output = [0xa5; 4_096];
        let output_pointer = output.as_ptr();
        let compiled = compile(
            request(
                language,
                source,
                ToolchainSelection::ResolvedNative(resolved),
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
        )
        .map_err(|failure| TestFailure::CompileTerminal {
            tool,
            expected: CompileExpectation::CompactFact,
            observed: compile_terminal(&failure),
        })?;
        if Language::from(compiled.recipe.profile) != language {
            return Err(TestFailure::RecipeLanguage {
                tool,
                expected: language,
                actual: Language::from(compiled.recipe.profile),
            });
        }
        if !core::ptr::eq(compiled.fragment.as_ref().as_ptr(), output_pointer) {
            return Err(TestFailure::FragmentBorrow { tool });
        }
        assert_facts(&compiled.fragment, expected)?;
        assert_type_facts(&compiled.fragment, tool)?;
        native_work.assert_empty()?;
    }
    Ok(())
}

#[test]
fn direct_python_semantic_ir_binds_the_exact_source_profile() -> Result<(), TestFailure> {
    let profile = LanguageProfile::Python(PythonVersion::Python314);
    let source = b"PROFILE_BOUND = True\n";
    let tool = NativeTool::Python;
    let executable = executable(tool)?;
    let toolchain = resolved(tool, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 4_096];
    let compiled = compile_ir(
        request(
            Language::Python,
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
    if compiled.ir.storage_columns().authority != SemanticImageAuthority::Language(profile) {
        return Err(TestFailure::SemanticAuthority {
            expected: profile,
            actual: compiled.ir.storage_columns().authority,
        });
    }
    native_work.assert_empty()
}
