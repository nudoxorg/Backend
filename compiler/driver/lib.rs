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
pub use native::typescript::coords::{
    AuthorityOrigin, SourceSpan, SourceSpanError, Utf8Span, Utf16Offset, utf16_span_to_utf8,
};
pub use native::{
    ClangDiagnostic, ClangDiagnosticSeverity, ClangFailure, ClangPhase, ClangSourceSpan,
};
pub use types::{
    CompileControl, CompileFailure, CompileOutput, CompileRecipeFact, CompileRequest,
    CompileScratch, CompiledFragment, InvalidUtf8Fact, MAX_NATIVE_WORKER_PANIC_BYTES,
    NativeDiagnostic, NativeTool, NativeWorkError, NativeWorkPrimary, NativeWorker,
    NativeWorkerPanic, NativeWorkerPanicClass, NativeWorkerPanicMessage, ResolvedToolchain,
    ResolvedToolchainView, SourceIdentity, ToolchainResolutionError, ToolchainSelection,
    ToolchainSelectionFact, compile,
};
