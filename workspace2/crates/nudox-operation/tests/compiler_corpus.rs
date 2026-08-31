//! Executable deterministic multilingual compiler corpus.

#[path = "support/multilingual_corpus.rs"]
mod multilingual_corpus;
#[path = "support/native_tooling.rs"]
mod native_tooling;

use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use multilingual_corpus::{
    CorpusLanguage, CorpusPackage, CorpusRenderError, PACKAGE_COUNT, SOURCE_BYTE_LIMIT,
    corpus_packages,
};
use native_tooling::{HostTool, NativeToolingError, NativeWork};
use nudox_compile_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch,
    CompiledFragment, NativeTool, ResolvedToolchain, ToolchainSelection, compile,
};
use nudox_compile_vocab::{Language, Stage};
use nudox_id::{ContentId, SourceFactDomain};
use nudox_ir_format::{EntityKind, PrimitiveType, SourceIdentity, TypeNode};
use thiserror::Error;

const DIAGNOSTIC_BYTES: usize = 4_096;
const FRAGMENT_BYTES: usize = 512;
const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
enum CorpusCompileError {
    #[error(transparent)]
    Render(#[from] CorpusRenderError),
    #[error(transparent)]
    Tooling(#[from] NativeToolingError),
    #[error("corpus package {ordinal} did not produce the required native compiler output")]
    ExpectedNativeOutput { ordinal: usize },
    #[error("corpus package {ordinal} did not produce the exact typed unavailable terminal")]
    ExpectedUnavailable { ordinal: usize },
    #[error("corpus package {ordinal} source length {observed} exceeded the typed u32 fact")]
    SourceLength { ordinal: usize, observed: usize },
}

#[test]
fn all_two_hundred_ten_packages_reach_a_real_adapter_or_exact_typed_terminal()
-> Result<(), CorpusCompileError> {
    let rust = HostTool::resolve("rustc", NativeTool::Rustc)?;
    let python = HostTool::resolve("python3", NativeTool::Python)?;
    let clang = HostTool::resolve("clang", NativeTool::Clang)?;
    let resolved = ResolvedTools {
        rust: rust.toolchain()?,
        python: python.toolchain()?,
        clang: clang.toolchain()?,
    };
    let mut supported = 0;
    let mut unavailable = 0;

    for package in corpus_packages() {
        match run_package(package, resolved)? {
            CorpusOutcome::Compiled => supported += 1,
            CorpusOutcome::Unavailable => unavailable += 1,
        }
    }
    assert_eq!(CorpusLanguage::ALL.len(), 7);
    assert_eq!(supported, 90);
    assert_eq!(unavailable, 120);
    assert_eq!(supported + unavailable, PACKAGE_COUNT);
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CorpusOutcome {
    Compiled,
    Unavailable,
}

fn run_package(
    package: CorpusPackage,
    resolved: ResolvedTools<'_>,
) -> Result<CorpusOutcome, CorpusCompileError> {
    let mut source_output = [0xa5; SOURCE_BYTE_LIMIT];
    let rendered = package.render(&mut source_output)?;
    let work = NativeWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic_output = [0; DIAGNOSTIC_BYTES];
    let mut fragment_output = [0xa5; FRAGMENT_BYTES];
    let (language, toolchain) = selection(package.language, resolved);
    let result = compile(
        CompileRequest {
            language,
            stage: Stage::LowerIr,
            source: rendered.source.as_bytes(),
            toolchain,
            control: CompileControl {
                deadline: Instant::now() + DEADLINE,
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic_output,
            native_work: work.path(),
        },
        CompileOutput {
            fragment_output: &mut fragment_output,
        },
    );
    let outcome = match expected_tool(package.language) {
        ExpectedTool::Resolved => {
            let compiled = result.ok();
            assert!(compiled.is_some(), "corpus package {}", package.ordinal);
            let Some(compiled) = compiled else {
                return Err(CorpusCompileError::ExpectedNativeOutput {
                    ordinal: package.ordinal,
                });
            };
            assert_compiled(&compiled, rendered.expected_symbol.as_bytes());
            CorpusOutcome::Compiled
        }
        ExpectedTool::Unavailable(tool) => {
            assert_unavailable(
                &result,
                package.ordinal,
                rendered.source.as_bytes(),
                language,
                tool,
            )?;
            drop(result);
            assert!(fragment_output.iter().all(|byte| *byte == 0xa5));
            CorpusOutcome::Unavailable
        }
    };
    work.assert_empty()?;
    Ok(outcome)
}

#[derive(Clone, Copy)]
struct ResolvedTools<'path> {
    rust: ResolvedToolchain<'path>,
    python: ResolvedToolchain<'path>,
    clang: ResolvedToolchain<'path>,
}

const fn selection(
    language: CorpusLanguage,
    resolved: ResolvedTools<'_>,
) -> (Language, ToolchainSelection<'_>) {
    match language {
        CorpusLanguage::Rust => (
            Language::Rust,
            ToolchainSelection::ResolvedNative(resolved.rust),
        ),
        CorpusLanguage::TypeScript => (
            Language::TypeScript,
            ToolchainSelection::ExplicitlyUnavailable {
                tool: NativeTool::TypeScriptCompiler,
            },
        ),
        CorpusLanguage::Python => (
            Language::Python,
            ToolchainSelection::ResolvedNative(resolved.python),
        ),
        CorpusLanguage::Go => (
            Language::Go,
            ToolchainSelection::ExplicitlyUnavailable {
                tool: NativeTool::GoCompiler,
            },
        ),
        CorpusLanguage::Java => (
            Language::Java,
            ToolchainSelection::ExplicitlyUnavailable {
                tool: NativeTool::JavaCompiler,
            },
        ),
        CorpusLanguage::CSharp => (
            Language::CSharp,
            ToolchainSelection::ExplicitlyUnavailable {
                tool: NativeTool::CSharpCompiler,
            },
        ),
        CorpusLanguage::Clang => (
            Language::Clang,
            ToolchainSelection::ResolvedNative(resolved.clang),
        ),
    }
}

#[derive(Clone, Copy)]
enum ExpectedTool {
    Resolved,
    Unavailable(NativeTool),
}

const fn expected_tool(language: CorpusLanguage) -> ExpectedTool {
    match language {
        CorpusLanguage::Rust | CorpusLanguage::Python | CorpusLanguage::Clang => {
            ExpectedTool::Resolved
        }
        CorpusLanguage::TypeScript => ExpectedTool::Unavailable(NativeTool::TypeScriptCompiler),
        CorpusLanguage::Go => ExpectedTool::Unavailable(NativeTool::GoCompiler),
        CorpusLanguage::Java => ExpectedTool::Unavailable(NativeTool::JavaCompiler),
        CorpusLanguage::CSharp => ExpectedTool::Unavailable(NativeTool::CSharpCompiler),
    }
}

fn assert_compiled(compiled: &CompiledFragment<'_>, expected_symbol: &[u8]) {
    assert!(
        compiled
            .fragment
            .entities()
            .map(|entity| (entity.kind, entity.name.raw))
            .eq([(EntityKind::Constant, 0)])
    );
    assert!(
        compiled
            .fragment
            .atoms()
            .map(|atom| atom.bytes)
            .eq([expected_symbol])
    );
    assert!(
        compiled
            .fragment
            .type_nodes()
            .eq([TypeNode::Primitive(PrimitiveType::String)])
    );
}

fn assert_unavailable(
    result: &Result<CompiledFragment<'_>, CompileFailure<'_>>,
    ordinal: usize,
    source: &[u8],
    language: Language,
    tool: NativeTool,
) -> Result<(), CorpusCompileError> {
    let byte_len =
        u32::try_from(source.len()).map_err(|_source| CorpusCompileError::SourceLength {
            ordinal,
            observed: source.len(),
        })?;
    let expected_source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source),
        byte_len,
    };
    let exact_terminal = matches!(
        result,
        Err(CompileFailure::ToolingUnavailable {
            source_identity,
            language: observed_language,
            stage,
            tool: observed_tool,
        }) if *source_identity == expected_source
            && *observed_language == language
            && *stage == Stage::LowerIr
            && *observed_tool == tool
    );
    assert!(exact_terminal, "corpus package {ordinal}");
    if exact_terminal {
        Ok(())
    } else {
        Err(CorpusCompileError::ExpectedUnavailable { ordinal })
    }
}
