//! Defines types behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the types invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Public caller authority plus closed compile terminals and compact output facts.

mod authority;
mod compile;
mod lowering;
mod request;
mod terminal;
mod toolchain;

pub use authority::{
    AuthorityDiagnostic, AuthorityDiagnosticFault, AuthorityFailure, AuthorityFailureProjection,
    AuthorityProfileMismatch,
};
pub use compile::{compile, compile_ir, compile_semantic};
pub use lowering::{
    ClangProjectionFault, FactFault, FactRejection, ParentageState, SourceSpanFact, TypeChildLane,
};
pub use request::{
    CompileControl, CompileRequest, CompileScratch, DeclarationScope, PackageDeclarationScopeFault,
    SemanticAuthorityInput,
};
pub(crate) use request::{NativeRecipe, SourceLease, WorkPermit, WorkStopped};
pub use terminal::{
    CompileFailure, CompileOutput, CompiledFragment, CompiledIr, CompiledSemantic,
    NativeDiagnostic, NativeWorkError, NativeWorkPrimary,
};
pub use toolchain::{
    ResolvedToolchain, ResolvedToolchainView, ToolchainResolutionError, ToolchainSelection,
    ToolchainSelectionFact,
};

pub use compiler_ir::SourceIdentity;
pub use compiler_vocabulary::{
    CompileRecipeFact, InvalidUtf8Fact, LoweringUnsupported, MAX_NATIVE_WORKER_PANIC_BYTES,
    NativeArtifactRole, NativeTool, NativeWorkPhase, NativeWorker, NativeWorkerPanic,
    NativeWorkerPanicClass, NativeWorkerPanicMessage,
};
