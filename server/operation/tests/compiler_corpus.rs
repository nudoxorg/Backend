//! Exercises the `server-operation` tests compiler-corpus contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Executable deterministic multilingual compiler corpus.

#[path = "support/multilingual_corpus.rs"]
mod multilingual_corpus;
#[path = "support/native_tooling.rs"]
mod native_tooling;

use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch,
    CompiledFragment, NativeTool, ResolvedToolchain, ToolchainSelection, compile,
};
use compiler_ir::{SourceIdentity, TypeNode};
use compiler_vocabulary::{
    CSharpVersion, CStandard, GoVersion, JavaRelease, Language, LanguageProfile, PythonVersion,
    RustEdition, Stage, TypeScriptSource,
};
use heart_identity::{
    ArtifactHasher, ContentId, IrFragmentDomain, IrFragmentEncoding, SourceFactDomain,
    ToolchainDomain,
};
use multilingual_corpus::{
    CorpusLanguage, CorpusPackage, CorpusRenderError, PACKAGE_COUNT, SOURCE_BYTE_LIMIT,
    corpus_packages,
};
use native_tooling::{HostTool, NativeToolingError, NativeWork};
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
    #[error("corpus package {ordinal} source length {observed} exceeded the typed u32 fact")]
    SourceLength {
        ordinal: usize,
        observed: usize,
        #[source]
        source: std::num::TryFromIntError,
    },
    #[error("corpus package {ordinal} failed for {language:?} with {tool:?}: {terminal:?}")]
    Compile {
        ordinal: usize,
        language: Language,
        tool: NativeTool,
        source_identity: SourceIdentity,
        terminal: CompileTerminalKind,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompileTerminalKind {
    SourceLength,
    UnsupportedStage,
    ToolchainSelectionMismatch,
    ToolchainMismatch,
    NativeWork,
    NativeWorkCleanup,
    ToolingUnavailable,
    ToolStart,
    MissingToolInput,
    MissingToolInputCleanup,
    MissingToolDiagnostic,
    MissingToolDiagnosticCleanup,
    ToolInput,
    ToolInputCleanup,
    ToolTerminate,
    ToolWait,
    ToolWaitCleanup,
    ToolDiagnosticRead,
    ToolDiagnosticReadCleanup,
    NativeWorkerPanic,
    Cancelled,
    DeadlineExceeded,
    DiagnosticLimit,
    NativeRejected,
    LoweringUnsupported,
    Build,
    Prepare,
    Write,
    Validate,
}

#[test]
fn all_two_hundred_ten_packages_reach_a_real_adapter_or_exact_typed_terminal()
-> Result<(), CorpusCompileError> {
    let rust = HostTool::resolve(NativeTool::Rustc)?;
    let python = HostTool::resolve(NativeTool::Python)?;
    let clang = HostTool::resolve(NativeTool::Clang)?;
    let typescript = HostTool::resolve(NativeTool::TypeScriptCompiler)?;
    let go = HostTool::resolve(NativeTool::GoCompiler)?;
    let java = HostTool::resolve(NativeTool::JavaCompiler)?;
    let csharp = HostTool::resolve(NativeTool::CSharpCompiler)?;
    let resolved = ResolvedTools {
        rust: rust.toolchain()?,
        python: python.toolchain()?,
        clang: clang.toolchain()?,
        typescript: typescript.toolchain()?,
        go: go.toolchain()?,
        java: java.toolchain()?,
        csharp: csharp.toolchain()?,
    };
    let first = run_corpus(&resolved)?;
    let second = run_corpus(&resolved)?;
    assert_eq!(first, second);
    assert_eq!(CorpusLanguage::ALL.len(), 7);
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CorpusRecord {
    source: [u8; 32],
    recipe: [u8; 32],
    fragment: [u8; 32],
    language: Language,
    tool: NativeTool,
}

type FragmentDigest = heart_identity::ArtifactId<IrFragmentEncoding, IrFragmentDomain>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CorpusRun {
    digest: FragmentDigest,
}

fn run_corpus(resolved: &ResolvedTools<'_>) -> Result<CorpusRun, CorpusCompileError> {
    let mut digest = ArtifactHasher::<IrFragmentEncoding, IrFragmentDomain>::new();
    let mut previous: Option<CorpusRecord> = None;
    let mut observed = 0;
    for package in corpus_packages() {
        let record = run_package(package, resolved)?;
        assert_eq!(record.language, language(package.language));
        assert_eq!(record.tool, native_tool(package.language));
        if let Some(previous) = previous {
            assert_ne!(record.source, previous.source);
            assert_ne!(record.recipe, previous.recipe);
            assert_ne!(record.fragment, previous.fragment);
        }
        digest.write_chunk(&record.source);
        digest.write_chunk(&record.recipe);
        digest.write_chunk(&record.fragment);
        previous = Some(record);
        observed += 1;
    }
    assert_eq!(observed, PACKAGE_COUNT);
    Ok(CorpusRun {
        digest: digest.finalize(),
    })
}

fn run_package(
    package: CorpusPackage,
    resolved: &ResolvedTools<'_>,
) -> Result<CorpusRecord, CorpusCompileError> {
    let mut source_output = [0xa5; SOURCE_BYTE_LIMIT];
    let rendered = package.render(&mut source_output)?;
    let work = NativeWork::create()?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic_output = [0; DIAGNOSTIC_BYTES];
    let mut fragment_output = [0xa5; FRAGMENT_BYTES];
    let (profile, toolchain) = selection(package.language, resolved);
    let result = compile(
        CompileRequest {
            profile,
            stage: Stage::LowerIr,
            source: rendered.source.as_bytes(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
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
    let source_identity = source_identity(rendered.source.as_bytes(), package.ordinal)?;
    let tool = native_tool(package.language);
    let compiled = result.map_err(|failure| CorpusCompileError::Compile {
        ordinal: package.ordinal,
        language: profile.language(),
        tool,
        source_identity,
        terminal: terminal_kind(&failure),
    })?;
    let record = assert_compiled(
        &compiled,
        &rendered,
        source_identity,
        toolchain.identity,
        profile.language(),
        tool,
    );
    work.assert_empty()?;
    Ok(record)
}

const fn native_tool(language: CorpusLanguage) -> NativeTool {
    match language {
        CorpusLanguage::Rust => NativeTool::Rustc,
        CorpusLanguage::TypeScript => NativeTool::TypeScriptCompiler,
        CorpusLanguage::Python => NativeTool::Python,
        CorpusLanguage::Go => NativeTool::GoCompiler,
        CorpusLanguage::Java => NativeTool::JavaCompiler,
        CorpusLanguage::CSharp => NativeTool::CSharpCompiler,
        CorpusLanguage::Clang => NativeTool::Clang,
    }
}

const fn language(corpus_language: CorpusLanguage) -> Language {
    match corpus_language {
        CorpusLanguage::Rust => Language::Rust,
        CorpusLanguage::TypeScript => Language::TypeScript,
        CorpusLanguage::Python => Language::Python,
        CorpusLanguage::Go => Language::Go,
        CorpusLanguage::Java => Language::Java,
        CorpusLanguage::CSharp => Language::CSharp,
        CorpusLanguage::Clang => Language::Clang,
    }
}

#[derive(Clone, Copy)]
struct ResolvedTools<'path> {
    rust: ResolvedToolchain<'path>,
    python: ResolvedToolchain<'path>,
    clang: ResolvedToolchain<'path>,
    typescript: ResolvedToolchain<'path>,
    go: ResolvedToolchain<'path>,
    java: ResolvedToolchain<'path>,
    csharp: ResolvedToolchain<'path>,
}

const fn selection<'path>(
    language: CorpusLanguage,
    resolved: &ResolvedTools<'path>,
) -> (LanguageProfile, ResolvedToolchain<'path>) {
    match language {
        CorpusLanguage::Rust => (LanguageProfile::Rust(RustEdition::Rust2024), resolved.rust),
        CorpusLanguage::TypeScript => (
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            resolved.typescript,
        ),
        CorpusLanguage::Python => (
            LanguageProfile::Python(PythonVersion::Python314),
            resolved.python,
        ),
        CorpusLanguage::Go => (LanguageProfile::Go(GoVersion::Go125), resolved.go),
        CorpusLanguage::Java => (LanguageProfile::Java(JavaRelease::Java21), resolved.java),
        CorpusLanguage::CSharp => (
            LanguageProfile::CSharp(CSharpVersion::CSharp12),
            resolved.csharp,
        ),
        CorpusLanguage::Clang => (LanguageProfile::C(CStandard::C23), resolved.clang),
    }
}

fn assert_compiled(
    compiled: &CompiledFragment<'_>,
    expected: &multilingual_corpus::RenderedPackage<'_>,
    expected_source: SourceIdentity,
    expected_toolchain: ContentId<ToolchainDomain>,
    expected_language: Language,
    expected_tool: NativeTool,
) -> CorpusRecord {
    assert_eq!(compiled.source, expected_source);
    assert_eq!(compiled.recipe.profile.language(), expected_language);
    assert_eq!(compiled.recipe.stage, Stage::LowerIr);
    assert_eq!(compiled.recipe.tool, expected_tool);
    assert_eq!(compiled.recipe.toolchain, expected_toolchain);
    assert!(
        compiled
            .fragment
            .entities()
            .map(|entity| (entity.kind, entity.name.raw))
            .eq([(expected.expected_kind, 0)])
    );
    assert_eq!(
        compiled.fragment.atoms().next().map(|atom| atom.bytes),
        Some(expected.expected_symbol.as_bytes())
    );
    assert!(
        compiled
            .fragment
            .type_nodes()
            .eq([TypeNode::Primitive(expected.expected_type)])
    );
    let mut hasher = ArtifactHasher::<IrFragmentEncoding, IrFragmentDomain>::new();
    hasher.write_chunk(compiled.fragment.as_ref());
    CorpusRecord {
        source: *compiled.source.identity.as_ref(),
        recipe: *compiled.recipe.identity.as_ref(),
        fragment: *hasher.finalize().as_ref(),
        language: compiled.recipe.profile.language(),
        tool: compiled.recipe.tool,
    }
}

fn source_identity(source: &[u8], ordinal: usize) -> Result<SourceIdentity, CorpusCompileError> {
    let observed = source.len();
    let byte_len = u32::try_from(observed).map_err(|source| CorpusCompileError::SourceLength {
        ordinal,
        observed,
        source,
    })?;
    Ok(SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source),
        byte_len,
    })
}

const fn terminal_kind(failure: &CompileFailure<'_>) -> CompileTerminalKind {
    match failure {
        CompileFailure::SourceLength { .. } => CompileTerminalKind::SourceLength,
        CompileFailure::UnsupportedStage { .. } => CompileTerminalKind::UnsupportedStage,
        CompileFailure::ToolchainSelectionMismatch { .. } => {
            CompileTerminalKind::ToolchainSelectionMismatch
        }
        CompileFailure::ToolchainMismatch { .. } => CompileTerminalKind::ToolchainMismatch,
        CompileFailure::NativeWork { .. } => CompileTerminalKind::NativeWork,
        CompileFailure::NativeWorkCleanup { .. } => CompileTerminalKind::NativeWorkCleanup,
        CompileFailure::ToolingUnavailable { .. } => CompileTerminalKind::ToolingUnavailable,
        CompileFailure::ToolStart { .. } => CompileTerminalKind::ToolStart,
        CompileFailure::MissingToolInput { .. } => CompileTerminalKind::MissingToolInput,
        CompileFailure::MissingToolInputCleanup { .. } => {
            CompileTerminalKind::MissingToolInputCleanup
        }
        CompileFailure::MissingToolDiagnostic { .. } => CompileTerminalKind::MissingToolDiagnostic,
        CompileFailure::MissingToolDiagnosticCleanup { .. } => {
            CompileTerminalKind::MissingToolDiagnosticCleanup
        }
        CompileFailure::ToolInput { .. } => CompileTerminalKind::ToolInput,
        CompileFailure::ToolInputCleanup { .. } => CompileTerminalKind::ToolInputCleanup,
        CompileFailure::ToolTerminate { .. } => CompileTerminalKind::ToolTerminate,
        CompileFailure::ToolWait { .. } => CompileTerminalKind::ToolWait,
        CompileFailure::ToolWaitCleanup { .. } => CompileTerminalKind::ToolWaitCleanup,
        CompileFailure::ToolDiagnosticRead { .. } => CompileTerminalKind::ToolDiagnosticRead,
        CompileFailure::ToolDiagnosticReadCleanup { .. } => {
            CompileTerminalKind::ToolDiagnosticReadCleanup
        }
        CompileFailure::NativeWorkerPanic { .. } => CompileTerminalKind::NativeWorkerPanic,
        CompileFailure::Cancelled { .. } => CompileTerminalKind::Cancelled,
        CompileFailure::DeadlineExceeded { .. } => CompileTerminalKind::DeadlineExceeded,
        CompileFailure::DiagnosticLimit { .. } => CompileTerminalKind::DiagnosticLimit,
        CompileFailure::NativeRejected { .. } => CompileTerminalKind::NativeRejected,
        CompileFailure::LoweringUnsupported { .. } => CompileTerminalKind::LoweringUnsupported,
        CompileFailure::Build { .. } => CompileTerminalKind::Build,
        CompileFailure::Prepare { .. } => CompileTerminalKind::Prepare,
        CompileFailure::Write { .. } => CompileTerminalKind::Write,
        CompileFailure::Validate { .. } => CompileTerminalKind::Validate,
    }
}
