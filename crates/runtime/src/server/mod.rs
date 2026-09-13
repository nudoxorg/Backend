//! The `backend_runtime::server` module schedules bounded work with explicit credits, ownership, and wakeups.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![deny(unsafe_op_in_unsafe_fn)]
//! Physical-credit MPMC admission and one-owner execution.
//!
//! [`Admission`] is a borrowed, scoped producer handle: no `Arc` extends runtime lifetime. Every
//! successful admission holds one work permit, one terminal permit, and quantized byte-credit
//! permits in its concrete queue message. [`Owner`] alone drains messages and terminal facts.

mod admission;
mod admission_bundle;
mod async_admission;
mod budget;
mod initialized_prefix;
mod metrics;
mod owner;
mod payload_slot;
mod probe;
mod ready_bitmap;
mod runtime;
mod slot;
mod storage;
mod waiter;
mod work_permit;

pub use self::admission::{Admission, AdmissionError, RejectedWork, RejectionReason};
pub use self::async_admission::{AdmissionCancellation, AdmissionFuture};
pub use self::budget::{
    BoundedWork, ByteBudget, ByteBudgetError, ByteBudgetFacts, ByteQuantum, RetainedBytes,
};
pub use self::metrics::{
    Accounting, AtomicAccounting, ObservedAccounting, RuntimeHistory, RuntimeMetrics, TerminalClass,
};
pub use self::owner::{Owner, OwnerFault, OwnerProgress, TerminalEvent, TerminalOutcome};
#[doc(hidden)]
pub use self::payload_slot::PayloadSlot;
pub use self::probe::{
    RuntimeAdmission, RuntimeContainment, RuntimeExecution, RuntimeProbeEvent, RuntimeTerminal,
};
pub use self::runtime::{
    InlineRuntime, LocalRuntime, RemoteRuntime, Runtime, RuntimeConfigError, RuntimeSplit,
};
pub use self::slot::{CancelResult, SlotClaimError, SlotIndex, WorkHandle};
pub use self::storage::{InlineStorage, LocalRuntimeArena, LocalStorage, RemoteStorage, RuntimeStorage};
pub use self::waiter::WaiterRegistrationError;
#[doc(hidden)]
pub use self::waiter::WaiterSlot;
#[doc(hidden)]
pub use self::waiter::{InlineWaiterTable, LocalWaiterTable, RemoteWaiterTable, WaiterTable};

/// Result of constructing one concrete bounded admission future.
pub type AdmissionWaitResult<
    'runtime,
    Generation,
    Work,
    AccountingPolicy = (),
    StoragePolicy = RemoteStorage,
    Failure = core::convert::Infallible,
> = Result<
    AdmissionFuture<'runtime, Generation, Work, AccountingPolicy, StoragePolicy, Failure>,
    AdmissionError<Work>,
>;

#[cfg(test)]
pub(crate) mod test_report {
    use std::string::String;

    /// Concrete test failure used when a scoped or Loom thread unwinds unexpectedly.
    #[derive(Debug, thiserror::Error)]
    #[error("thread unwound with panic payload {payload:?}")]
    pub(crate) struct ThreadPanic {
        payload: PanicPayload,
    }

    #[derive(Debug, Eq, PartialEq)]
    enum PanicPayload {
        StaticMessage(&'static str),
        OwnedMessage(String),
        Other,
    }

    pub(crate) fn join<Output>(result: std::thread::Result<Output>) -> Result<Output, ThreadPanic> {
        result.map_err(|payload| {
            let payload = match payload.downcast::<&'static str>() {
                Ok(message) => PanicPayload::StaticMessage(*message),
                Err(payload) => match payload.downcast::<String>() {
                    Ok(message) => PanicPayload::OwnedMessage(*message),
                    Err(_payload) => PanicPayload::Other,
                },
            };
            ThreadPanic { payload }
        })
    }

    /// Converts a typed Loom scenario result into the single model-checker panic boundary.
    #[cfg(feature = "loom-model")]
    #[allow(
        clippy::panic,
        reason = "Loom reports a failed explored execution through this one test-only panic boundary"
    )]
    pub(crate) fn assert_loom<Output, Error: core::fmt::Display>(
        scenario: impl FnOnce() -> Result<Output, Error>,
    ) -> Output {
        match scenario() {
            Ok(value) => value,
            Err(error) => panic!("{error}"),
        }
    }
}

#[cfg(all(test, not(feature = "loom-model")))]
mod runtime_tests;
