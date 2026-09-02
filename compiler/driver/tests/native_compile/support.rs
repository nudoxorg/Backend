//! Exercises the `compiler-driver` tests native-compile support contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::Instant,
};

use compiler_driver::{
    CompileControl, CompileFailure, CompileRequest, LoweringUnsupported, NativeTool,
    NativeWorkPhase, NativeWorkPrimary, ResolvedToolchain, ToolchainResolutionError,
    ToolchainSelection,
};
use compiler_ir::SemanticImageAuthority;
use compiler_ir::{EntityKind, PrimitiveType};
use compiler_vocabulary::{
    CSharpVersion, CStandard, GoVersion, JavaRelease, Language, LanguageProfile, PythonVersion,
    RustEdition, Stage, TypeScriptSource,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub(super) enum TestFailure {
    #[error("host does not expose the required {tool:?} executable")]
    MissingHostTool { tool: NativeTool },
    #[error(
        "host does not expose the explicit Node.js executable for the TypeScript authority fixture"
    )]
    MissingTypeScriptNode,
    #[error("TypeScript authority fixture path was not valid UTF-8")]
    TypeScriptFixturePath,
    #[error("could not materialize the exact TypeScript authority fixture")]
    TypeScriptFixtureWrite(#[source] std::io::Error),
    #[error("could not canonicalize the required {tool:?} executable")]
    Canonicalize {
        tool: NativeTool,
        #[source]
        cause: std::io::Error,
    },
    #[error("could not probe the exact {tool:?} compiler version")]
    ProbeVersion {
        tool: NativeTool,
        #[source]
        cause: std::io::Error,
    },
    #[error("the exact {tool:?} compiler version probe was rejected with {status}")]
    VersionRejected {
        tool: NativeTool,
        status: ExitStatus,
    },
    #[error("the exact {tool:?} compiler version probe returned no identity bytes")]
    EmptyVersion { tool: NativeTool },
    #[error(transparent)]
    Resolve(#[from] ToolchainResolutionError),
    #[error("fixture source length {actual} does not fit the compact source fact")]
    SourceLength {
        actual: usize,
        #[source]
        cause: core::num::TryFromIntError,
    },
    #[error("could not create the caller-owned native work directory")]
    CreateWork(#[source] std::io::Error),
    #[error("could not inspect the caller-owned native work directory")]
    InspectWork(#[source] std::io::Error),
    #[error("the caller-owned native work directory was not clean after native reaping")]
    WorkNotEmpty,
    #[error(
        "the locally reproducible {tool:?} adapter was expected to {expected:?}, but ended at {observed:?}"
    )]
    CompileTerminal {
        tool: NativeTool,
        expected: CompileExpectation,
        observed: CompileTerminal,
    },
    #[error(
        "the locally reproducible {tool:?} adapter bound {actual:?}, not {expected:?}, as recipe language"
    )]
    RecipeLanguage {
        tool: NativeTool,
        expected: Language,
        actual: Language,
    },
    #[error(
        "the locally reproducible {tool:?} adapter bound {actual:?}, not {expected:?}, as recipe stage"
    )]
    RecipeStage {
        tool: NativeTool,
        expected: Stage,
        actual: Stage,
    },
    #[error(
        "the locally reproducible {tool:?} adapter bound {actual:?}, not {expected:?}, as recipe tool"
    )]
    RecipeTool {
        tool: NativeTool,
        expected: NativeTool,
        actual: NativeTool,
    },
    #[error(
        "the locally reproducible {tool:?} adapter bound source length {actual}, not {expected}"
    )]
    FragmentSourceLength {
        tool: NativeTool,
        expected: u32,
        actual: u32,
    },
    #[error("the locally reproducible {tool:?} adapter returned a fragment outside caller output")]
    FragmentBorrow { tool: NativeTool },
    #[error(
        "the locally reproducible {tool:?} adapter atom fact had length {actual:?}, not {expected}"
    )]
    AtomLength {
        tool: NativeTool,
        expected: usize,
        actual: Option<usize>,
    },
    #[error(
        "the locally reproducible {tool:?} adapter declaration kind was {actual:?}, not {expected:?}"
    )]
    EntityKind {
        tool: NativeTool,
        expected: EntityKind,
        actual: Option<EntityKind>,
    },
    #[error(
        "the adapter emitted {actual_count} entities, not the expected {expected_count}; the first differing entity row is {ordinal}"
    )]
    EntitySequence {
        ordinal: usize,
        expected_count: usize,
        actual_count: usize,
    },
    #[error("the {tool:?} adapter emitted no decodable type-fact section")]
    TypeFactsMissing { tool: NativeTool },
    #[error("the {tool:?} adapter type-fact section failed to decode")]
    TypeFactsUndecodable { tool: NativeTool },
    #[error(
        "the locally reproducible {tool:?} adapter primitive type fact was {actual:?}, not {expected:?}"
    )]
    PrimitiveType {
        tool: NativeTool,
        expected: PrimitiveType,
        actual: Option<PrimitiveType>,
    },
    #[error("the locally reproducible {tool:?} adapter changed output bytes after its fragment")]
    OutputTailChanged { tool: NativeTool },
    #[error("the locally reproducible {tool:?} native rejection emitted no diagnostic bytes")]
    EmptyNativeDiagnostic { tool: NativeTool },
    #[error(
        "the locally reproducible {tool:?} native rejection unexpectedly exceeded its diagnostic lease"
    )]
    TruncatedNativeDiagnostic { tool: NativeTool },
    #[error(
        "the locally reproducible {tool:?} native rejection observed {actual}, not {expected}, diagnostic bytes"
    )]
    NativeDiagnosticObserved {
        tool: NativeTool,
        expected: usize,
        actual: usize,
    },
    #[error("the native fixture expected distinct {fact:?} values")]
    ExpectedDistinctFact { fact: FragmentFact },
    #[error(
        "the native fixture expected a closed toolchain-resolution terminal, observed successful resolution"
    )]
    ResolutionUnexpectedlySucceeded,
    #[error("the rebound toolchain view was not exactly the caller-proven TypeScript authority")]
    ReboundToolchainMismatch,
    #[error("semantic IR authority was {actual:?}, not exact profile {expected:?}")]
    SemanticAuthority {
        expected: LanguageProfile,
        actual: SemanticImageAuthority,
    },
    #[error("semantic IR retained {actual} declaration rows, not {expected}")]
    SemanticEntityCount { expected: usize, actual: usize },
    #[error("semantic IR retained {actual} {kind:?} rows, not {expected}")]
    SemanticItemKindCount {
        kind: compiler_ir::ItemKind,
        expected: usize,
        actual: usize,
    },
    #[error("semantic IR visibility was {actual:?}, not unproven {expected:?}")]
    SemanticVisibility {
        expected: compiler_ir::Visibility,
        actual: compiler_ir::Visibility,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FragmentFact {
    Bytes,
    SourceIdentity,
    RecipeIdentity,
    ToolchainIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CompileExpectation {
    CompactFact,
    NativeRejection,
    LoweringUnsupported(LoweringUnsupported),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CompileTerminal {
    Compiled,
    SourceLength,
    UnsupportedStage,
    ToolchainSelectionMismatch,
    ToolchainMismatch,
    NativeWork(NativeWorkPhase),
    NativeWorkCleanup(NativeWorkPrimaryTerminal),
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
    Authority,
    LoweringUnsupported(LoweringUnsupported),
    Build,
    Prepare,
    Write,
    Validate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeWorkPrimaryTerminal {
    Prepare,
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
    WorkerPanic,
    Cancelled,
    DeadlineExceeded,
    DiagnosticLimit,
    NativeRejected,
}

static WORK_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
pub(super) const STABLE_TOOLCHAIN_ENV: &str = "COMPILER_STABLE_TOOLCHAIN";
pub(super) const TYPESCRIPT_COMPILER_ENV: &str = "COMPILER_TYPESCRIPT_COMPILER";
pub(super) const CSHARP_COMPILER_ENV: &str = "COMPILER_CSHARP_COMPILER";
pub(super) const GO_COMPILER_ENV: &str = "COMPILER_GO_COMPILER";
pub(super) const JAVA_COMPILER_ENV: &str = "COMPILER_JAVA_COMPILER";

pub(super) struct TemporaryWork {
    path: PathBuf,
}

impl TemporaryWork {
    pub(super) fn create() -> Result<Self, TestFailure> {
        let sequence = WORK_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!("compiler-work-{}-{sequence}", std::process::id()));
        fs::create_dir(&path).map_err(TestFailure::CreateWork)?;
        Ok(Self { path })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn assert_empty(&self) -> Result<(), TestFailure> {
        let mut entries = fs::read_dir(&self.path).map_err(TestFailure::InspectWork)?;
        match entries.next() {
            Some(Ok(_entry)) => Err(TestFailure::WorkNotEmpty),
            Some(Err(cause)) => Err(TestFailure::InspectWork(cause)),
            None => Ok(()),
        }
    }

    #[cfg(unix)]
    pub(super) fn write_typescript_runner(
        &self,
        node: &Path,
        compiler: &Path,
    ) -> Result<PathBuf, TestFailure> {
        use std::os::unix::fs::PermissionsExt;

        let node = node.to_str().ok_or(TestFailure::TypeScriptFixturePath)?;
        let compiler = compiler
            .to_str()
            .ok_or(TestFailure::TypeScriptFixturePath)?;
        let runner = self.path.join("typescript-authority-runner");
        let body = format!("#!/bin/sh\nexec {node} {compiler} \"$@\"\n");
        fs::write(&runner, body).map_err(TestFailure::TypeScriptFixtureWrite)?;
        let mut permissions = fs::metadata(&runner)
            .map_err(TestFailure::TypeScriptFixtureWrite)?
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&runner, permissions).map_err(TestFailure::TypeScriptFixtureWrite)?;
        Ok(runner)
    }
}

impl Drop for TemporaryWork {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.path);
    }
}

pub(super) fn executable(tool: NativeTool) -> Result<PathBuf, TestFailure> {
    if tool == NativeTool::Rustc
        && let Some(toolchain_root) = env::var_os(STABLE_TOOLCHAIN_ENV)
    {
        let candidate = PathBuf::from(toolchain_root).join("bin/rustc");
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .map_err(|cause| TestFailure::Canonicalize { tool, cause });
        }
    }
    if let Some(configured) = configured_executable(tool) {
        return configured
            .canonicalize()
            .map_err(|cause| TestFailure::Canonicalize { tool, cause });
    }
    let name = match tool {
        NativeTool::Rustc => "rustc",
        NativeTool::Python => "python3",
        NativeTool::Clang => "clang",
        NativeTool::TypeScriptCompiler
        | NativeTool::CSharpCompiler
        | NativeTool::GoCompiler
        | NativeTool::JavaCompiler => {
            return Err(TestFailure::MissingHostTool { tool });
        }
    };
    let paths = env::var_os("PATH").ok_or(TestFailure::MissingHostTool { tool })?;
    for directory in env::split_paths(&paths) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .map_err(|cause| TestFailure::Canonicalize { tool, cause });
        }
    }
    Err(TestFailure::MissingHostTool { tool })
}

pub(super) fn configured_executable(tool: NativeTool) -> Option<PathBuf> {
    let variable = match tool {
        NativeTool::TypeScriptCompiler => TYPESCRIPT_COMPILER_ENV,
        NativeTool::CSharpCompiler => CSHARP_COMPILER_ENV,
        NativeTool::GoCompiler => GO_COMPILER_ENV,
        NativeTool::JavaCompiler => JAVA_COMPILER_ENV,
        NativeTool::Rustc | NativeTool::Clang | NativeTool::Python => return None,
    };
    env::var_os(variable).map(PathBuf::from)
}

pub(super) fn typescript_node() -> Result<PathBuf, TestFailure> {
    let path = env::var_os("COMPILER_TYPESCRIPT_NODE").ok_or(TestFailure::MissingTypeScriptNode)?;
    PathBuf::from(path)
        .canonicalize()
        .map_err(TestFailure::TypeScriptFixtureWrite)
}

pub(super) fn resolved<'path>(
    tool: NativeTool,
    path: &'path Path,
) -> Result<ResolvedToolchain<'path>, TestFailure> {
    let version_argument = match tool {
        NativeTool::GoCompiler => "version",
        NativeTool::Rustc
        | NativeTool::Clang
        | NativeTool::Python
        | NativeTool::TypeScriptCompiler
        | NativeTool::JavaCompiler
        | NativeTool::CSharpCompiler => "--version",
    };
    let version = Command::new(path)
        .arg(version_argument)
        .output()
        .map_err(|cause| TestFailure::ProbeVersion { tool, cause })?;
    if !version.status.success() {
        return Err(TestFailure::VersionRejected {
            tool,
            status: version.status,
        });
    }
    let version_bytes = if version.stdout.is_empty() {
        version.stderr.as_slice()
    } else {
        version.stdout.as_slice()
    };
    if version_bytes.is_empty() {
        return Err(TestFailure::EmptyVersion { tool });
    }
    Ok(ResolvedToolchain::from_version(tool, path, version_bytes)?)
}

pub(super) fn source_length(source: &[u8]) -> Result<u32, TestFailure> {
    u32::try_from(source.len()).map_err(|cause| TestFailure::SourceLength {
        actual: source.len(),
        cause,
    })
}

/// Asserts the fragment's entity sequence is exactly `expected`: each row's
/// entity kind and resolved name atom must match positionally, and no extra
/// entity may exist.
#[allow(
    clippy::as_conversions,
    reason = "FragmentView validation proved every atom coordinate fits the test address space before this usize projection"
)]
pub(super) fn assert_facts(
    fragment: &compiler_ir::FragmentView<'_>,
    expected: &[(&'static [u8], EntityKind)],
) -> Result<(), TestFailure> {
    let atoms: Vec<&[u8]> = fragment.atoms().map(|atom| atom.bytes).collect();
    let entities: Vec<(usize, EntityKind)> = fragment
        .entities()
        .map(|entity| (entity.name.raw as usize, entity.kind))
        .collect();
    let ordinal =
        entities
            .iter()
            .zip(expected)
            .position(|((name, kind), (expected_name, expected_kind))| {
                atoms[*name] != *expected_name || kind != expected_kind
            });
    if entities.len() != expected.len() {
        return Err(TestFailure::EntitySequence {
            ordinal: ordinal.unwrap_or(expected.len()),
            expected_count: expected.len(),
            actual_count: entities.len(),
        });
    }
    if let Some(ordinal) = ordinal {
        return Err(TestFailure::EntitySequence {
            ordinal,
            expected_count: expected.len(),
            actual_count: entities.len(),
        });
    }
    Ok(())
}

/// The interim driver seam must commit at least one type fact for every
/// successful language lowering; this catches deletion of the emission block.
pub(super) fn assert_type_facts(
    fragment: &compiler_ir::FragmentView<'_>,
    tool: NativeTool,
) -> Result<(), TestFailure> {
    let mut facts = fragment
        .type_facts()
        .ok_or(TestFailure::TypeFactsMissing { tool })?;
    match facts.next() {
        Some(Ok(_)) => Ok(()),
        Some(Err(_)) | None => Err(TestFailure::TypeFactsUndecodable { tool }),
    }
}

pub(super) fn compile_terminal(failure: &CompileFailure<'_>) -> CompileTerminal {
    match failure {
        CompileFailure::SourceLength { .. } => CompileTerminal::SourceLength,
        CompileFailure::UnsupportedStage { .. } => CompileTerminal::UnsupportedStage,
        CompileFailure::ToolchainSelectionMismatch { .. } => {
            CompileTerminal::ToolchainSelectionMismatch
        }
        CompileFailure::ToolchainMismatch { .. } => CompileTerminal::ToolchainMismatch,
        CompileFailure::NativeWork { phase, .. } => CompileTerminal::NativeWork(*phase),
        CompileFailure::NativeWorkCleanup { primary, .. } => {
            CompileTerminal::NativeWorkCleanup(native_work_primary_terminal(primary))
        }
        CompileFailure::ToolingUnavailable { .. } => CompileTerminal::ToolingUnavailable,
        CompileFailure::ToolStart { .. } => CompileTerminal::ToolStart,
        CompileFailure::MissingToolInput { .. } => CompileTerminal::MissingToolInput,
        CompileFailure::MissingToolInputCleanup { .. } => CompileTerminal::MissingToolInputCleanup,
        CompileFailure::MissingToolDiagnostic { .. } => CompileTerminal::MissingToolDiagnostic,
        CompileFailure::MissingToolDiagnosticCleanup { .. } => {
            CompileTerminal::MissingToolDiagnosticCleanup
        }
        CompileFailure::ToolInput { .. } => CompileTerminal::ToolInput,
        CompileFailure::ToolInputCleanup { .. } => CompileTerminal::ToolInputCleanup,
        CompileFailure::ToolTerminate { .. } => CompileTerminal::ToolTerminate,
        CompileFailure::ToolWait { .. } => CompileTerminal::ToolWait,
        CompileFailure::ToolWaitCleanup { .. } => CompileTerminal::ToolWaitCleanup,
        CompileFailure::ToolDiagnosticRead { .. } => CompileTerminal::ToolDiagnosticRead,
        CompileFailure::ToolDiagnosticReadCleanup { .. } => {
            CompileTerminal::ToolDiagnosticReadCleanup
        }
        CompileFailure::NativeWorkerPanic { .. } => CompileTerminal::NativeWorkerPanic,
        CompileFailure::Cancelled { .. } => CompileTerminal::Cancelled,
        CompileFailure::DeadlineExceeded { .. } => CompileTerminal::DeadlineExceeded,
        CompileFailure::DiagnosticLimit { .. } => CompileTerminal::DiagnosticLimit,
        CompileFailure::NativeRejected { .. } => CompileTerminal::NativeRejected,
        CompileFailure::Authority { .. } => CompileTerminal::Authority,
        CompileFailure::LoweringUnsupported { cause, .. } => {
            CompileTerminal::LoweringUnsupported(*cause)
        }
        CompileFailure::Build { .. } => CompileTerminal::Build,
        CompileFailure::Prepare { .. } => CompileTerminal::Prepare,
        CompileFailure::Write { .. } => CompileTerminal::Write,
        CompileFailure::Validate { .. } => CompileTerminal::Validate,
    }
}

pub(super) fn native_work_primary_terminal(
    primary: &NativeWorkPrimary<'_>,
) -> NativeWorkPrimaryTerminal {
    match primary {
        NativeWorkPrimary::Prepare { .. } => NativeWorkPrimaryTerminal::Prepare,
        NativeWorkPrimary::ToolStart { .. } => NativeWorkPrimaryTerminal::ToolStart,
        NativeWorkPrimary::MissingToolInput => NativeWorkPrimaryTerminal::MissingToolInput,
        NativeWorkPrimary::MissingToolInputCleanup { .. } => {
            NativeWorkPrimaryTerminal::MissingToolInputCleanup
        }
        NativeWorkPrimary::MissingToolDiagnostic => {
            NativeWorkPrimaryTerminal::MissingToolDiagnostic
        }
        NativeWorkPrimary::MissingToolDiagnosticCleanup { .. } => {
            NativeWorkPrimaryTerminal::MissingToolDiagnosticCleanup
        }
        NativeWorkPrimary::ToolInput { .. } => NativeWorkPrimaryTerminal::ToolInput,
        NativeWorkPrimary::ToolInputCleanup { .. } => NativeWorkPrimaryTerminal::ToolInputCleanup,
        NativeWorkPrimary::ToolTerminate { .. } => NativeWorkPrimaryTerminal::ToolTerminate,
        NativeWorkPrimary::ToolWait { .. } => NativeWorkPrimaryTerminal::ToolWait,
        NativeWorkPrimary::ToolWaitCleanup { .. } => NativeWorkPrimaryTerminal::ToolWaitCleanup,
        NativeWorkPrimary::ToolDiagnosticRead { .. } => {
            NativeWorkPrimaryTerminal::ToolDiagnosticRead
        }
        NativeWorkPrimary::ToolDiagnosticReadCleanup { .. } => {
            NativeWorkPrimaryTerminal::ToolDiagnosticReadCleanup
        }
        NativeWorkPrimary::WorkerPanic { .. } => NativeWorkPrimaryTerminal::WorkerPanic,
        NativeWorkPrimary::Cancelled { .. } => NativeWorkPrimaryTerminal::Cancelled,
        NativeWorkPrimary::DeadlineExceeded { .. } => NativeWorkPrimaryTerminal::DeadlineExceeded,
        NativeWorkPrimary::DiagnosticLimit { .. } => NativeWorkPrimaryTerminal::DiagnosticLimit,
        NativeWorkPrimary::NativeRejected { .. } => NativeWorkPrimaryTerminal::NativeRejected,
    }
}

pub(super) fn request<'source, 'path, 'cancel>(
    language: Language,
    source: &'source [u8],
    toolchain: ToolchainSelection<'path>,
    cancelled: &'cancel AtomicBool,
    deadline: Instant,
) -> CompileRequest<'source, 'path, 'cancel> {
    CompileRequest {
        profile: fixture_profile(language),
        stage: Stage::LowerIr,
        source,
        toolchain,
        control: CompileControl {
            deadline,
            cancelled,
        },
    }
}

const fn fixture_profile(language: Language) -> LanguageProfile {
    match language {
        Language::Rust => LanguageProfile::Rust(RustEdition::Rust2024),
        Language::TypeScript => LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        Language::Python => LanguageProfile::Python(PythonVersion::Python314),
        Language::Go => LanguageProfile::Go(GoVersion::Go125),
        Language::Java => LanguageProfile::Java(JavaRelease::Java21),
        Language::CSharp => LanguageProfile::CSharp(CSharpVersion::CSharp12),
        Language::Clang => LanguageProfile::C(CStandard::C23),
    }
}
