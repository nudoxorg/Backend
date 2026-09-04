//! The `compiler-driver` crate exists to run bounded native toolchains and lower their output into canonical IR.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Native parse admission followed by compact validated-source IR lowering.
#![allow(
    clippy::result_large_err,
    reason = "CompileFailure preserves public source, recipe, bounded diagnostic, and concrete I/O causes; boxing that terminal would add a default allocation to every compiler error path."
)]

mod lower;
mod native;
mod types;

pub use compiler_vocabulary::{LoweringUnsupported, NativeArtifactRole, NativeWorkPhase};
pub use types::{
    AuthorityDiagnostic, AuthorityDiagnosticFault, AuthorityFailure, AuthorityFailureProjection,
    AuthorityProfileMismatch, CompileControl, CompileFailure, CompileOutput, CompileRecipeFact,
    CompileRequest, CompileScratch, CompiledFragment, CompiledIr, DeclarationScope, FactFault, FactRejection,
    InvalidUtf8Fact, MAX_NATIVE_WORKER_PANIC_BYTES, NativeDiagnostic, NativeTool, NativeWorkError,
    NativeWorkPrimary, NativeWorker, NativeWorkerPanic, NativeWorkerPanicClass,
    NativeWorkerPanicMessage, ResolvedToolchain, ResolvedToolchainView, SemanticAuthorityInput,
    SourceIdentity, SourceSpanFact, ToolchainResolutionError, ToolchainSelection, ToolchainSelectionFact, compile,
    compile_ir,
};
