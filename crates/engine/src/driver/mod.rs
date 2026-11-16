//! The `backend-engine` driver module runs bounded native toolchains and lowers their output into canonical IR.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Native parse admission followed by compact validated-source IR lowering.
#![allow(
    clippy::result_large_err,
    reason = "CompileFailure preserves public source, recipe, bounded diagnostic, and concrete I/O causes; boxing that terminal would add a default allocation to every compiler error path."
)]

#[cfg(feature = "paused-frontends")]
mod database;
mod lower;
#[cfg(all(test, unix, feature = "paused-frontends"))]
mod native;
mod types;
#[cfg(feature = "paused-frontends")]
pub(crate) use self::types::go_authority_projection;

#[cfg(feature = "paused-frontends")]
pub use self::database::{DatabaseCompileFailure, compile_database_translation_unit};
pub(crate) use self::types::rust_authority_diagnostic;
pub use self::types::{
    AuthorityDiagnostic, AuthorityDiagnosticFault, AuthorityFailure, AuthorityFailureProjection,
    AuthorityProfileMismatch, CompileControl, CompileFailure, CompileOutput, CompileRecipeFact,
    CompileRequest, CompileScratch, CompiledFragment, CompiledIr, CompiledSemantic,
    DeclarationScope, ExternalToolchain, FactFault, FactRejection, InvalidUtf8Fact,
    MAX_NATIVE_WORKER_PANIC_BYTES, NativeDiagnostic, NativeInvocation, NativeInvocationError,
    NativeInvocationFileRole, NativeTool, NativeWorkError, NativeWorkPrimary, NativeWorker,
    NativeWorkerPanic, NativeWorkerPanicClass, NativeWorkerPanicMessage, NotExternalToolchain,
    PackageDeclarationScopeFault, ParentageState, ResolvedToolchain, ResolvedToolchainView,
    SemanticAuthorityInput, SourceIdentity, SourceSpanFact, ToolchainResolutionError,
    ToolchainSelection, ToolchainSelectionFact, TypeChildLane, compile, compile_ir,
    compile_semantic,
};
pub use backend_semantic::vocabulary::{LoweringUnsupported, NativeArtifactRole, NativeWorkPhase};

#[cfg(feature = "paused-frontends")]
pub use self::types::ClangProjectionFault;
