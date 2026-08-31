//! Independent native compiler-work oracle split by semantic fact family.

mod io;
mod terminal;
mod work;
mod worker;

pub use io::{GoldenErrorKind, GoldenNativeIoFact, GoldenNativeIoPhase};
pub use terminal::GoldenNativePrimaryCause;
pub use work::{
    GoldenNativeArtifactAction, GoldenNativeArtifactCause, GoldenNativeArtifactRole,
    GoldenNativeDirectoryCause, GoldenNativeWorkCause, GoldenNativeWorkCleanupCause,
    GoldenNativeWorkPhase,
};
pub use worker::{
    GoldenNativeWorker, GoldenNativeWorkerPanic, GoldenNativeWorkerPanicClass,
    GoldenNativeWorkerPanicMessage,
};
