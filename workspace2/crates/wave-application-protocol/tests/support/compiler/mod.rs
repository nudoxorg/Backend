//! Independent compiler/publication oracle used by adapter process tests.

mod authority;
mod native;
mod publication;
mod terminal;

pub use authority::{
    GoldenCompileRecipe, GoldenGeneratedArtifact, GoldenGenerationAuthority, GoldenLanguage,
    GoldenNativeTool, GoldenPublicationAuthority, GoldenSourceAuthority, GoldenStage,
};
#[allow(unused_imports)]
pub use native::{
    GoldenErrorKind, GoldenNativeArtifactAction, GoldenNativeArtifactCause,
    GoldenNativeArtifactRole, GoldenNativeDirectoryCause, GoldenNativeIoFact, GoldenNativeIoPhase,
    GoldenNativePrimaryCause, GoldenNativeWorkCause, GoldenNativeWorkCleanupCause,
    GoldenNativeWorkPhase, GoldenNativeWorker, GoldenNativeWorkerPanic,
    GoldenNativeWorkerPanicClass, GoldenNativeWorkerPanicMessage,
};
pub use publication::{GoldenPublicationCause, GoldenPublicationPhase};
pub use terminal::{
    GoldenCompilerAttempt, GoldenCompilerCause, GoldenCompilerDiagnostic, GoldenCompilerTerminal,
    GoldenFragmentCause, GoldenLoweringCause,
};
