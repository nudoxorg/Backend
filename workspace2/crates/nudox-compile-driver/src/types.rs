//! Public caller authority plus closed compile terminals and compact output facts.

mod compile;
mod request;
mod terminal;
mod toolchain;

pub use compile::compile;
pub(crate) use request::NativeRecipe;
pub use request::{CompileControl, CompileRequest, CompileScratch};
pub use terminal::{
    CompileFailure, CompileOutput, CompiledFragment, NativeDiagnostic, NativeWorkError,
    NativeWorkPrimary,
};
pub use toolchain::{
    ResolvedToolchain, ResolvedToolchainView, ToolchainResolutionError, ToolchainSelection,
    ToolchainSelectionFact,
};

pub use nudox_compile_vocab::{
    CompileRecipeFact, InvalidUtf8Fact, LoweringUnsupported, MAX_NATIVE_WORKER_PANIC_BYTES,
    NativeArtifactRole, NativeTool, NativeWorkPhase, NativeWorker, NativeWorkerPanic,
    NativeWorkerPanicClass, NativeWorkerPanicMessage,
};
pub use nudox_ir_format::SourceIdentity;
