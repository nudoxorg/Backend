//! Exercises semantic-admission behavior through the public compiler boundary.
//! Replaces retired source-scanner expectations with exact authority and durable-admission proof.
//! Keeps every failure source typed, source-bound, and independent of host PATH discovery.

use std::{
    num::NonZeroUsize,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileFailure, CompileOutput, CompileScratch, NativeTool, ToolchainSelection, compile,
};
use compiler_ir::EntityKind;
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use backend_semantic::vocabulary::Language;
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use thiserror::Error;

use super::support::*;

#[allow(
    clippy::large_enum_variant,
    reason = "the integration proof retains each exact durable publication and reopen terminal without erasing it behind a test-only box"
)]
#[derive(Debug, Error)]
enum TypeScriptPublicationError {
    #[error(transparent)]
    Compile(#[from] TestFailure),
    #[error("TypeScript publication fixture could not create an artifact directory")]
    ArtifactDirectory(#[source] std::io::Error),
    #[error("TypeScript publication fixture could not create a journal directory")]
    JournalDirectory(#[source] std::io::Error),
    #[error("TypeScript publication limit was rejected")]
    Limits(#[source] server_journal::PublicationLimitError),
    #[error("TypeScript publication owner could not open")]
    Publisher(#[source] server_journal::PublicationOpenError),
    #[error("TypeScript compact fragment could not publish")]
    Publish(#[source] compiler_publication::PublishCompiledError),
    #[error("TypeScript compact publication could not reopen")]
    Open(#[source] compiler_publication::OpenPublishedError),
    #[error("TypeScript publication reopen returned no durable compilation")]
    MissingPublication,
    #[error("TypeScript publication reopen returned no fragment")]
    MissingFragment,
    #[error("TypeScript publication reopen returned an invalid fragment")]
    Fragment(#[source] compiler_publication::OpenedFragmentError),
    #[error("TypeScript publication owner could not shut down")]
    Shutdown(#[source] server_journal::ShutdownError),
}

#[test]
fn external_profiles_require_bound_authority_images_before_any_native_work()
-> Result<(), TestFailure> {
    let cases = [
        (
            Language::Go,
            NativeTool::GoCompiler,
            b"package fixture\nfunc GoFact() {}\n".as_slice(),
        ),
        (
            Language::CSharp,
            NativeTool::CSharpCompiler,
            b"public interface CSharpFact {}\n".as_slice(),
        ),
        (
            Language::Java,
            NativeTool::JavaCompiler,
            b"public interface JavaFact {}\n".as_slice(),
        ),
    ];
    for (language, tool, source) in cases {
        let toolchain = direct_authority_toolchain(tool)?;
        let native_work = TemporaryWork::create()?;
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0xa5; 128];
        let mut output = [0xa5; 512];
        match compile(
            request(
                language,
                source,
                ToolchainSelection::ResolvedNative(toolchain),
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
            Err(CompileFailure::AuthorityInputRequired { profile, .. })
                if Language::from(profile) == language => {}
            Err(failure) => {
                return Err(TestFailure::CompileTerminal {
                    tool,
                    expected: CompileExpectation::AuthorityInputRequired,
                    observed: compile_terminal(&failure),
                });
            }
            Ok(_) => {
                return Err(TestFailure::CompileTerminal {
                    tool,
                    expected: CompileExpectation::AuthorityInputRequired,
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
fn direct_oxc_bindings_fill_the_compact_fragment_without_a_tsc_spawn() -> Result<(), TestFailure> {
    let tool = NativeTool::TypeScriptCompiler;
    let toolchain = direct_authority_toolchain(tool)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 128];
    let mut output = [0xa5; 4_096];
    let compiled = compile(
        request(
            Language::TypeScript,
            b"export interface Shape { area(): number; } export const value = 1;",
            ToolchainSelection::ResolvedNative(toolchain),
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
    )
    .map_err(|failure| TestFailure::CompileTerminal {
        tool,
        expected: CompileExpectation::CompactFact,
        observed: compile_terminal(&failure),
    })?;
    assert_facts(
        &compiled.fragment,
        &[
            (b"Shape", EntityKind::Trait),
            (b"number", EntityKind::Alias),
            (b"area", EntityKind::Function),
            (b"value", EntityKind::Constant),
        ],
    )?;
    assert_type_facts(&compiled.fragment, tool)?;
    native_work.assert_empty()
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "the integration proof returns exact durable publication and reopen terminals by value so a failing test preserves their operands"
)]
fn direct_oxc_compact_fragment_survives_durable_reopen() -> Result<(), TypeScriptPublicationError> {
    let tool = NativeTool::TypeScriptCompiler;
    let toolchain = direct_authority_toolchain(tool)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 128];
    let mut output = [0xa5; 4_096];
    let compiled = compile(
        request(
            Language::TypeScript,
            b"export interface ReopenedShape {} export const reopened = 1;",
            ToolchainSelection::ResolvedNative(toolchain),
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
    )
    .map_err(|failure| TestFailure::CompileTerminal {
        tool,
        expected: CompileExpectation::CompactFact,
        observed: compile_terminal(&failure),
    })?;
    native_work.assert_empty()?;
    let publication_root = TemporaryWork::create()?;
    let artifacts = publication_root.path().join("artifacts");
    let journal = publication_root.path().join("journal");
    std::fs::create_dir(&artifacts).map_err(TypeScriptPublicationError::ArtifactDirectory)?;
    std::fs::create_dir(&journal).map_err(TypeScriptPublicationError::JournalDirectory)?;
    let limits = PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
        .map_err(TypeScriptPublicationError::Limits)?;
    let publisher = DurablePublisher::create(&PublicationPaths::in_directory(&journal), limits)
        .map_err(TypeScriptPublicationError::Publisher)?;
    let mut manifest_output = [0; 4_096];
    let mut manifest_facts = [None; 1];
    let mut ordinals = [0; 1];
    let mut locality_output = [0; 4_096];
    let mut binding_output = [0; compiler_publication::binding::COMPILATION_BINDING_BYTES];
    publish_compiled(
        &publisher,
        &artifacts,
        core::slice::from_ref(&compiled),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest_output,
            manifest_facts: &mut manifest_facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality_output,
            binding_output: &mut binding_output,
        },
    )
    .map_err(TypeScriptPublicationError::Publish)?;
    let mut reopened_manifest = [0; 4_096];
    let mut reopened_facts = [None; 1];
    let mut reopened_fragments = [0; 4_096];
    let mut reopened_locality = [0; 4_096];
    let Some(reopened) = open_published(
        &publisher,
        &artifacts,
        OpenPublicationScratch {
            manifest_output: &mut reopened_manifest,
            manifest_facts: &mut reopened_facts,
            fragment_output: &mut reopened_fragments,
            locality_output: &mut reopened_locality,
        },
    )
    .map_err(TypeScriptPublicationError::Open)?
    else {
        return Err(TypeScriptPublicationError::MissingPublication);
    };
    let Some(fragment) = reopened.fragments().next() else {
        return Err(TypeScriptPublicationError::MissingFragment);
    };
    let fragment = fragment.map_err(TypeScriptPublicationError::Fragment)?;
    assert_facts(
        &fragment.view,
        &[
            (b"ReopenedShape", EntityKind::Trait),
            (b"reopened", EntityKind::Constant),
        ],
    )?;
    publisher
        .shutdown()
        .map_err(TypeScriptPublicationError::Shutdown)
}

#[test]
fn rust_without_a_project_never_falls_back_to_source_lowering() -> Result<(), TestFailure> {
    let source = b"pub const alpha: u64 = { const INNER: bool = true; 1 };";
    let executable = executable(NativeTool::Rustc)?;
    let toolchain = resolved(NativeTool::Rustc, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 4_096];
    let mut output = [0xa5; 4_096];
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
        Err(CompileFailure::AuthorityInputRequired { .. }) => {}
        Err(failure) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Rustc,
                expected: CompileExpectation::AuthorityInputRequired,
                observed: compile_terminal(&failure),
            });
        }
        Ok(_) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Rustc,
                expected: CompileExpectation::AuthorityInputRequired,
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
fn one_more_than_the_fact_lane_capacity_never_uses_a_rust_source_fallback()
-> Result<(), TestFailure> {
    let mut source = String::new();
    for ordinal in 0..=128 {
        source.push_str(&format!("pub static CAPACITY_{ordinal}: bool = true;\n"));
    }
    let executable = executable(NativeTool::Rustc)?;
    let toolchain = resolved(NativeTool::Rustc, &executable)?;
    let native_work = TemporaryWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0xa5; 4_096];
    let mut output = [0xa5; 4_096];
    match compile(
        request(
            Language::Rust,
            source.as_bytes(),
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
        Err(CompileFailure::AuthorityInputRequired { .. }) => {}
        Err(failure) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Rustc,
                expected: CompileExpectation::AuthorityInputRequired,
                observed: compile_terminal(&failure),
            });
        }
        Ok(_) => {
            return Err(TestFailure::CompileTerminal {
                tool: NativeTool::Rustc,
                expected: CompileExpectation::AuthorityInputRequired,
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
fn rust_authority_required_terminal_retains_each_source_identity() -> Result<(), TestFailure> {
    let alpha_source = b"pub const alpha: bool = true;";
    let bravo_source = b"pub const bravo: i32 = 1;";
    let executable = executable(NativeTool::Rustc)?;
    let toolchain = resolved(NativeTool::Rustc, &executable)?;
    let cancelled = AtomicBool::new(false);
    let mut alpha_diagnostic = [0xa5; 4_096];
    let mut bravo_diagnostic = [0xa5; 4_096];
    let mut alpha_output = [0xa5; 4_096];
    let mut bravo_output = [0xa5; 4_096];
    let alpha_work = TemporaryWork::create()?;
    let bravo_work = TemporaryWork::create()?;
    let alpha = authority_required(
        alpha_source,
        toolchain,
        &cancelled,
        &mut alpha_diagnostic,
        alpha_work.path(),
        &mut alpha_output,
    )?;
    let bravo = authority_required(
        bravo_source,
        toolchain,
        &cancelled,
        &mut bravo_diagnostic,
        bravo_work.path(),
        &mut bravo_output,
    )?;
    if alpha.0.identity == bravo.0.identity {
        return Err(TestFailure::ExpectedDistinctFact {
            fact: FragmentFact::Source,
        });
    }
    if alpha.1.identity == bravo.1.identity {
        return Err(TestFailure::ExpectedDistinctFact {
            fact: FragmentFact::Recipe,
        });
    }
    alpha_work.assert_empty()?;
    bravo_work.assert_empty()?;
    if !alpha_output.iter().all(|byte| *byte == 0xa5)
        || !bravo_output.iter().all(|byte| *byte == 0xa5)
    {
        return Err(TestFailure::OutputTailChanged {
            tool: NativeTool::Rustc,
        });
    }
    Ok(())
}

fn authority_required<'source, 'toolchain, 'cancel>(
    source: &'source [u8],
    toolchain: compiler_driver::ResolvedToolchain<'toolchain>,
    cancelled: &'cancel AtomicBool,
    diagnostic: &mut [u8],
    native_work: &std::path::Path,
    output: &mut [u8],
) -> Result<
    (
        compiler_driver::SourceIdentity,
        compiler_driver::CompileRecipeFact,
    ),
    TestFailure,
> {
    match compile(
        request(
            Language::Rust,
            source,
            ToolchainSelection::ResolvedNative(toolchain),
            cancelled,
            Instant::now() + Duration::from_secs(5),
        ),
        CompileScratch {
            diagnostic_output: diagnostic,
            native_work,
        },
        CompileOutput {
            fragment_output: output,
        },
    ) {
        Err(CompileFailure::AuthorityInputRequired {
            source_identity,
            recipe,
            ..
        }) => Ok((source_identity, recipe)),
        Err(failure) => Err(TestFailure::CompileTerminal {
            tool: NativeTool::Rustc,
            expected: CompileExpectation::AuthorityInputRequired,
            observed: compile_terminal(&failure),
        }),
        Ok(_) => Err(TestFailure::CompileTerminal {
            tool: NativeTool::Rustc,
            expected: CompileExpectation::AuthorityInputRequired,
            observed: CompileTerminal::Compiled,
        }),
    }
}
