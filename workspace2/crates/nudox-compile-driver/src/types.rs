use core::{num::TryFromIntError, ops::Deref};
use std::{path::Path, sync::atomic::AtomicBool, time::Instant};

use nudox_compile_registry::{AdapterRoute, FullRegistry};
use nudox_compile_vocab::{FrontendError, Language, Stage};
use nudox_id::{ContentId, SourceFactDomain, ToolchainDomain};
use nudox_ir_format::{
    AtomInput, EntityRecord, FragmentError, FragmentView, PrepareError, PreparedFragment, TypeNode,
    WriteError,
};
use nudox_ir_vocab::{AtomId, TypeId};
use thiserror::Error;

use crate::{lower::declaration, native::parse_with_native_tool};

pub use nudox_compile_vocab::{CompileRecipeFact, NativeTool};
pub use nudox_ir_format::SourceIdentity;

/// Immutable, caller-resolved native executable facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedToolchainView {
    /// Concrete tool family whose command line the caller resolved.
    pub tool: NativeTool,
    /// Central typed identity derived from caller-supplied exact version bytes.
    pub identity: ContentId<ToolchainDomain>,
}

/// Caller-resolved absolute executable capability with private execution authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedToolchain<'path> {
    view: ResolvedToolchainView,
    executable: &'path Path,
}

impl<'path> Deref for ResolvedToolchain<'path> {
    type Target = ResolvedToolchainView;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

/// Rejection while binding an explicit executable to its version provenance.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ToolchainResolutionError {
    /// Relative executable lookup would consult ambient process search state.
    #[error("resolved executable path is not absolute")]
    RelativeExecutable,
}

impl<'path> ResolvedToolchain<'path> {
    /// Binds one absolute executable path to exact caller-probed tool version bytes.
    pub fn from_version(
        tool: NativeTool,
        executable: &'path Path,
        version_bytes: &[u8],
    ) -> Result<Self, ToolchainResolutionError> {
        if !executable.is_absolute() {
            return Err(ToolchainResolutionError::RelativeExecutable);
        }
        let identity = ContentId::<ToolchainDomain>::from_canonical_bytes(version_bytes);
        Ok(Self {
            view: ResolvedToolchainView { tool, identity },
            executable,
        })
    }

    pub(crate) fn executable(self) -> &'path Path {
        self.executable
    }
}

/// Closed toolchain state supplied to a compilation request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolchainSelection<'path> {
    /// A validated absolute executable may service one locally reproducible native adapter.
    ResolvedNative(ResolvedToolchain<'path>),
    /// This language's selected native tool is intentionally unavailable without a forged path.
    ExplicitlyUnavailable { tool: NativeTool },
}

/// Exact shape of a supplied toolchain selection retained by mismatch diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolchainSelectionFact {
    /// Request supplied a resolved executable for this concrete tool family.
    ResolvedNative { tool: NativeTool },
    /// Request supplied an explicit unavailable terminal for this concrete tool family.
    ExplicitlyUnavailable { tool: NativeTool },
}

impl<'path> ToolchainSelection<'path> {
    fn fact(self) -> ToolchainSelectionFact {
        match self {
            Self::ResolvedNative(resolved) => ToolchainSelectionFact::ResolvedNative {
                tool: resolved.tool,
            },
            Self::ExplicitlyUnavailable { tool } => {
                ToolchainSelectionFact::ExplicitlyUnavailable { tool }
            }
        }
    }
}

/// Deadline and cancellation facts borrowed by one bounded native invocation.
#[derive(Clone, Copy, Debug)]
pub struct CompileControl<'cancel> {
    /// Monotonic deadline after which the native child is killed and reaped.
    pub deadline: Instant,
    /// Caller-owned cancellation flag observed before input and while waiting for the child.
    pub cancelled: &'cancel AtomicBool,
}

/// Immutable compile request borrowing recipe and cancellation authority from its caller.
#[derive(Clone, Copy, Debug)]
pub struct CompileRequest<'source, 'toolchain, 'cancel> {
    /// Closed language family selected at the static registry boundary.
    pub language: Language,
    /// Requested semantic terminal; only `LowerIr` can produce a compact IR fragment.
    pub stage: Stage,
    /// Exact UTF-8-or-binary source bytes whose identity is persisted only on native lowering.
    pub source: &'source [u8],
    /// Resolved native authority or explicit unavailable tool fact, never an ambient lookup.
    pub toolchain: ToolchainSelection<'toolchain>,
    /// Bounded cancellation and deadline control for native work.
    pub control: CompileControl<'cancel>,
}

/// Internal recipe after the closed registry has admitted a resolved native toolchain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NativeRecipe<'source, 'toolchain> {
    pub(crate) language: Language,
    pub(crate) stage: Stage,
    pub(crate) source: &'source [u8],
    pub(crate) toolchain: ResolvedToolchain<'toolchain>,
}

/// Reusable caller-owned diagnostic lease; it never aliases semantic IR output.
pub struct CompileScratch<'diagnostic, 'work> {
    /// Bounded native stderr capture; a limit breach kills and reaps the native child.
    pub diagnostic_output: &'diagnostic mut [u8],
    /// Explicit caller-owned empty work directory; adapters never inherit the repository cwd.
    pub native_work: &'work Path,
}

/// Phase owning an explicit caller-provided native work directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeWorkPhase {
    /// The adapter verified that its caller-owned directory is empty before spawn.
    Prepare,
    /// The adapter removed its exact known output and verified no files remained after reaping.
    Cleanup,
}

/// Validation or I/O failure for the bounded caller-owned native work directory.
#[derive(Debug, Error)]
pub enum NativeWorkError {
    /// Relative work paths would reintroduce ambient process working-directory state.
    #[error("native work directory is not absolute")]
    RelativeDirectory,
    /// The caller-owned work directory could not be inspected.
    #[error("could not inspect the native work directory")]
    Inspect(#[source] std::io::Error),
    /// Compilation may begin only with a known empty caller-owned work directory.
    #[error("native work directory was not empty")]
    NotEmpty,
    /// The exact Rust metadata output could not be removed after child reaping.
    #[error("could not remove the exact Rust metadata output")]
    RemoveMetadata(#[source] std::io::Error),
}

/// Exact native terminal retained when its post-reap work cleanup also fails.
#[derive(Debug)]
pub enum NativeWorkPrimary<'diagnostic> {
    /// Starting the selected native executable failed.
    ToolStart { cause: std::io::Error },
    /// Native child omitted its writable standard-input lease.
    MissingToolInput,
    /// Missing standard input also had a child-cleanup I/O failure.
    MissingToolInputCleanup { cleanup: std::io::Error },
    /// Native child omitted its readable standard-error lease.
    MissingToolDiagnostic,
    /// Missing standard error also had a child-cleanup I/O failure.
    MissingToolDiagnosticCleanup { cleanup: std::io::Error },
    /// Sending exact source to native standard input failed.
    ToolInput { cause: std::io::Error },
    /// Source input and child cleanup both had exact I/O failures.
    ToolInputCleanup {
        cause: std::io::Error,
        cleanup: std::io::Error,
    },
    /// Interrupting a native child failed.
    ToolTerminate { cause: std::io::Error },
    /// Polling a native child failed.
    ToolWait { cause: std::io::Error },
    /// Polling and required child cleanup both had exact I/O failures.
    ToolWaitCleanup {
        cause: std::io::Error,
        cleanup: std::io::Error,
    },
    /// Reading bounded native diagnostics failed.
    ToolDiagnosticRead { cause: std::io::Error },
    /// Cancellation reaped the child before a semantic result.
    Cancelled {
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Deadline expiry reaped the child before a semantic result.
    DeadlineExceeded {
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Diagnostic capacity reaped the child before a semantic result.
    DiagnosticLimit {
        limit: usize,
        observed: usize,
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Native parser rejected source after reaping.
    NativeRejected {
        status: std::process::ExitStatus,
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
}

/// Closed semantic terminal for syntax-native source whose declaration facts lack a supported IR recipe.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LoweringUnsupported {
    /// No declaration form in this language has a compact semantic recipe in this compiler slice.
    #[error("no supported declaration form")]
    NoSupportedDeclaration,
    /// Rust function signatures need a distinct semantic recipe and are not lowered as constants.
    #[error("Rust function recipe is not represented")]
    RustFunction,
    /// Rust constant type is outside the closed Bool/I32/String recipe set.
    #[error("Rust constant type is not represented")]
    RustConstantType,
    /// Python assignment has no nonempty identifier fact.
    #[error("Python assignment identifier is not represented")]
    PythonAssignmentName,
    /// Python assignment value is outside the closed Bool/I32/String recipe set.
    #[error("Python assignment value is not represented")]
    PythonAssignmentValue,
    /// Clang declaration is outside the closed `const char *` String recipe.
    #[error("Clang declaration form is not represented")]
    ClangDeclarationForm,
}

/// Separate caller-owned compact IR output region reused across compile calls.
pub struct CompileOutput<'output> {
    /// Output storage for the compact validated-source fragment.
    pub fragment_output: &'output mut [u8],
}

/// Borrowed bounded diagnostic emitted by a reaped native adapter terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeDiagnostic<'diagnostic> {
    /// Exact retained prefix in the caller-owned diagnostic lease.
    pub bytes: &'diagnostic [u8],
    /// Whether additional diagnostic bytes were drained without retention.
    pub truncated: bool,
}

/// Borrowed compact IR emitted only after native parser admission and typed lowering succeed.
pub struct CompiledFragment<'artifact> {
    /// Exact source fact persisted by the semantic fragment.
    pub source: SourceIdentity,
    /// Closed recipe fact bound to the returned lowering terminal.
    pub recipe: CompileRecipeFact,
    /// Validated compact IR borrowing only the separate caller-owned output region.
    pub fragment: FragmentView<'artifact>,
}

/// Exact compile terminal with source-bearing native causes and bounded diagnostic facts.
#[derive(Debug, Error)]
pub enum CompileFailure<'diagnostic> {
    /// The source length could not fit the compact source identity width.
    #[error("source has {actual} bytes, which exceeds the compact source identity width")]
    SourceLength {
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
    /// The registry makes this requested semantic stage an explicit terminal.
    #[error("{language:?} {stage:?} does not support the requested semantic terminal")]
    UnsupportedStage {
        source_identity: SourceIdentity,
        language: Language,
        stage: Stage,
        cause: FrontendError,
    },
    /// The request's closed selection cannot service the static registry row.
    #[error("{language:?} {stage:?} selected {selected:?}, not {provided:?}")]
    ToolchainSelectionMismatch {
        source_identity: SourceIdentity,
        language: Language,
        stage: Stage,
        selected: NativeTool,
        provided: ToolchainSelectionFact,
    },
    /// A resolved native executable does not match the closed registry tool selection.
    #[error("{language:?} {stage:?} resolved {resolved:?}, not selected {selected:?}")]
    ToolchainMismatch {
        source_identity: SourceIdentity,
        language: Language,
        stage: Stage,
        selected: NativeTool,
        resolved: NativeTool,
    },
    /// The caller-owned native work directory rejected this exact invocation phase.
    #[error("{recipe:?} native work {phase:?} failed")]
    NativeWork {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        phase: NativeWorkPhase,
        #[source]
        cause: NativeWorkError,
    },
    /// A native terminal and cleanup failure are retained together without erasing either cause.
    #[error("{recipe:?} completed its native terminal but native work cleanup also failed")]
    NativeWorkCleanup {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        primary: NativeWorkPrimary<'diagnostic>,
        #[source]
        cleanup: NativeWorkError,
    },
    /// The selected language has no locally reproducible native tooling adapter.
    #[error("{language:?} {stage:?} has no locally reproducible {tool:?} adapter")]
    ToolingUnavailable {
        source_identity: SourceIdentity,
        language: Language,
        stage: Stage,
        tool: NativeTool,
    },
    /// Starting the native parser process preserved its concrete I/O cause.
    #[error("could not start {recipe:?}")]
    ToolStart {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
    },
    /// The native process did not provide a writable standard input lease.
    #[error("{recipe:?} did not expose standard input")]
    MissingToolInput {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
    },
    /// The child lacked stdin and its required cleanup also failed.
    #[error("{recipe:?} did not expose standard input and cleanup failed")]
    MissingToolInputCleanup {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cleanup: std::io::Error,
    },
    /// The native process did not provide a readable standard error lease.
    #[error("{recipe:?} did not expose standard error")]
    MissingToolDiagnostic {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
    },
    /// The child lacked stderr and its required cleanup also failed.
    #[error("{recipe:?} did not expose standard error and cleanup failed")]
    MissingToolDiagnosticCleanup {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cleanup: std::io::Error,
    },
    /// Writing exact borrowed source to native stdin preserved its concrete I/O cause.
    #[error("could not send source for {recipe:?}")]
    ToolInput {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
    },
    /// Source input failed and the required child cleanup retained a second concrete I/O cause.
    #[error("could not send source for {recipe:?}; cleanup also failed")]
    ToolInputCleanup {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
        cleanup: std::io::Error,
    },
    /// Killing an interrupted native child preserved its concrete I/O cause.
    #[error("could not terminate {recipe:?}")]
    ToolTerminate {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
    },
    /// Waiting for the native parser process preserved its concrete I/O cause.
    #[error("could not observe {recipe:?}")]
    ToolWait {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
    },
    /// Polling the child failed and required cleanup preserved a second concrete I/O cause.
    #[error("could not observe {recipe:?}; cleanup also failed")]
    ToolWaitCleanup {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
        cleanup: std::io::Error,
    },
    /// The bounded diagnostic reader retained its concrete I/O cause.
    #[error("could not read bounded native diagnostics for {recipe:?}")]
    ToolDiagnosticRead {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: std::io::Error,
    },
    /// Cancellation killed and reaped the child before a semantic result became visible.
    #[error("{recipe:?} was cancelled")]
    Cancelled {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Deadline expiry killed and reaped the child before a semantic result became visible.
    #[error("{recipe:?} exceeded its deadline")]
    DeadlineExceeded {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Native stderr exceeded its caller-owned bounded lease and the child was killed and reaped.
    #[error("{recipe:?} exceeded diagnostic limit {limit} after {observed} bytes")]
    DiagnosticLimit {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        limit: usize,
        observed: usize,
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// The native parser rejected this exact source with bounded exact diagnostic facts.
    #[error("{recipe:?} rejected source with {status:?}")]
    NativeRejected {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        status: std::process::ExitStatus,
        diagnostic: NativeDiagnostic<'diagnostic>,
    },
    /// Native syntax passed but its declaration lacks a closed compact semantic recipe.
    #[error("{recipe:?} has an unsupported LowerIr declaration recipe")]
    LoweringUnsupported {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: LoweringUnsupported,
    },
    /// Lowering could not prepare the compact IR from its typed facts.
    #[error("could not prepare compact IR for {recipe:?}")]
    Prepare {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: PrepareError,
    },
    /// Caller output could not hold the prepared compact IR.
    #[error("could not write compact IR for {recipe:?}")]
    Write {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: WriteError,
    },
    /// Fresh compact IR failed its own borrowed validation.
    #[error("fresh compact IR failed validation for {recipe:?}")]
    Validate {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        #[source]
        cause: FragmentError,
    },
}

/// Parses exact source with one closed native adapter, then lowers its validated-source fact.
pub fn compile<'source, 'toolchain, 'cancel, 'diagnostic, 'work, 'output>(
    request: CompileRequest<'source, 'toolchain, 'cancel>,
    scratch: CompileScratch<'diagnostic, 'work>,
    output: CompileOutput<'output>,
) -> Result<CompiledFragment<'output>, CompileFailure<'diagnostic>> {
    let source = source_identity(request.source)?;
    let route = FullRegistry
        .route(request.language, request.stage)
        .map_err(|cause| CompileFailure::UnsupportedStage {
            source_identity: source,
            language: request.language,
            stage: request.stage,
            cause,
        })?;
    let selected = match route {
        AdapterRoute::Native { tool } => tool,
        AdapterRoute::ToolingUnavailable { tool } => {
            if request.toolchain.fact() != (ToolchainSelectionFact::ExplicitlyUnavailable { tool })
            {
                return Err(CompileFailure::ToolchainSelectionMismatch {
                    source_identity: source,
                    language: request.language,
                    stage: request.stage,
                    selected: tool,
                    provided: request.toolchain.fact(),
                });
            }
            return Err(CompileFailure::ToolingUnavailable {
                source_identity: source,
                language: request.language,
                stage: request.stage,
                tool,
            });
        }
    };
    let resolved = match request.toolchain {
        ToolchainSelection::ResolvedNative(resolved) => resolved,
        ToolchainSelection::ExplicitlyUnavailable { .. } => {
            return Err(CompileFailure::ToolchainSelectionMismatch {
                source_identity: source,
                language: request.language,
                stage: request.stage,
                selected,
                provided: request.toolchain.fact(),
            });
        }
    };
    if selected != resolved.tool {
        return Err(CompileFailure::ToolchainMismatch {
            source_identity: source,
            language: request.language,
            stage: request.stage,
            selected,
            resolved: resolved.tool,
        });
    }
    let recipe = recipe_fact(request.language, request.stage, resolved, source);
    let native_recipe = NativeRecipe {
        language: request.language,
        stage: request.stage,
        source: request.source,
        toolchain: resolved,
    };
    parse_with_native_tool(native_recipe, source, recipe, scratch, request.control)?;
    let declaration = declaration(request.language, request.source).map_err(|cause| {
        CompileFailure::LoweringUnsupported {
            source_identity: source,
            recipe,
            cause,
        }
    })?;
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: declaration.kind,
    }];
    let nodes = [TypeNode::Primitive(declaration.semantic_type)];
    let atoms = [AtomInput {
        bytes: declaration.name,
    }];
    let prepared =
        PreparedFragment::prepare(source, recipe, &entities, &nodes, &atoms).map_err(|cause| {
            CompileFailure::Prepare {
                source_identity: source,
                recipe,
                cause,
            }
        })?;
    let bytes = prepared
        .write_into(output.fragment_output)
        .map_err(|cause| CompileFailure::Write {
            source_identity: source,
            recipe,
            cause,
        })?;
    let fragment = FragmentView::validate(bytes).map_err(|cause| CompileFailure::Validate {
        source_identity: source,
        recipe,
        cause,
    })?;
    Ok(CompiledFragment {
        source,
        recipe,
        fragment,
    })
}

fn source_identity<'diagnostic>(
    source_bytes: &[u8],
) -> Result<SourceIdentity, CompileFailure<'diagnostic>> {
    let byte_len =
        u32::try_from(source_bytes.len()).map_err(|source| CompileFailure::SourceLength {
            actual: source_bytes.len(),
            source,
        })?;
    Ok(SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
        byte_len,
    })
}

fn recipe_fact(
    language: Language,
    stage: Stage,
    toolchain: ResolvedToolchain<'_>,
    source: SourceIdentity,
) -> CompileRecipeFact {
    CompileRecipeFact::derive(
        language,
        stage,
        toolchain.tool,
        source.identity,
        toolchain.identity,
    )
}
