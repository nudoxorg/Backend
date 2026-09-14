//! Exercises the `backend-library` tests support compiler contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Independent compiler/publication oracle used by adapter process tests.

mod authority;
mod lowering;
mod native;
mod publication;
mod terminal;

pub(crate) use authority::{
    GoldenCompileRecipe, GoldenGeneratedArtifact, GoldenGenerationAuthority, GoldenLanguage,
    GoldenNativeTool, GoldenPublicationAuthority, GoldenSourceAuthority, GoldenStage,
};
pub(crate) use lowering::GoldenLoweringCause;
#[allow(unused_imports)]
pub(crate) use native::{
    GoldenErrorKind, GoldenNativeArtifactAction, GoldenNativeArtifactCause,
    GoldenNativeArtifactRole, GoldenNativeDirectoryCause, GoldenNativeIoFact, GoldenNativeIoPhase,
    GoldenNativePrimaryCause, GoldenNativeWorkCause, GoldenNativeWorkCleanupCause,
    GoldenNativeWorkPhase, GoldenNativeWorker, GoldenNativeWorkerPanic,
    GoldenNativeWorkerPanicClass, GoldenNativeWorkerPanicMessage,
};
pub(crate) use publication::{GoldenPublicationCause, GoldenPublicationPhase};
pub(crate) use terminal::{
    GoldenCompilerAttempt, GoldenCompilerCause, GoldenCompilerDiagnostic, GoldenCompilerTerminal,
    GoldenFragmentCause,
};
