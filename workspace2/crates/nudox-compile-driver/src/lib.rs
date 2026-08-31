//! Native parse admission followed by compact validated-source IR lowering.
#![allow(
    clippy::result_large_err,
    reason = "CompileFailure preserves public source, recipe, bounded diagnostic, and concrete I/O causes; boxing that terminal would add a default allocation to every compiler error path."
)]

mod lower;
mod native;
mod types;

pub use nudox_compile_vocab::{LoweringUnsupported, NativeArtifactRole, NativeWorkPhase};
pub use types::{
    CompileControl, CompileFailure, CompileOutput, CompileRecipeFact, CompileRequest,
    CompileScratch, CompiledFragment, InvalidUtf8Fact, MAX_NATIVE_WORKER_PANIC_BYTES,
    NativeDiagnostic, NativeTool, NativeWorkError, NativeWorkPrimary, NativeWorker,
    NativeWorkerPanic, NativeWorkerPanicClass, NativeWorkerPanicMessage, ResolvedToolchain,
    ResolvedToolchainView, SourceIdentity, ToolchainResolutionError, ToolchainSelection,
    ToolchainSelectionFact, compile,
};
