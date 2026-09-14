//! Exercises the `backend-library` tests support compiler native contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Independent native compiler-work oracle split by semantic fact family.

mod io;
mod terminal;
mod work;
mod worker;

pub(crate) use io::{GoldenErrorKind, GoldenNativeIoFact, GoldenNativeIoPhase};
pub(crate) use terminal::GoldenNativePrimaryCause;
pub(crate) use work::{
    GoldenNativeArtifactAction, GoldenNativeArtifactCause, GoldenNativeArtifactRole,
    GoldenNativeDirectoryCause, GoldenNativeWorkCause, GoldenNativeWorkCleanupCause,
    GoldenNativeWorkPhase,
};
pub(crate) use worker::{
    GoldenNativeWorker, GoldenNativeWorkerPanic, GoldenNativeWorkerPanicClass,
    GoldenNativeWorkerPanicMessage,
};
