//! Defines lease cell behavior for `backend-semantic::graph_vector`, whose purpose is to execute typed graph and vector work through bounded leased storage.
//! This module owns the lease cell invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Checked access to the lease's shared payload cells.
//!
//! Production uses the standard library's raw-cell primitive. Loom builds use its instrumented
//! counterpart, which records every immutable and mutable access in the model. The
//! closure helpers deliberately keep references inside the access callback; the one persistent
//! read guard is used only for the public borrowed-batch lifetime.

#[cfg(not(all(test, feature = "loom-model")))]
pub(super) use core::cell::UnsafeCell;
#[cfg(all(test, feature = "loom-model"))]
pub(super) use loom::cell::UnsafeCell;

/// Runs an initialized immutable operation without allowing its borrow to escape the cell access.
pub(super) fn read<Payload: ?Sized, Output>(
    cell: &UnsafeCell<Payload>,
    operation: impl FnOnce(*const Payload) -> Output,
) -> Output {
    #[cfg(all(test, feature = "loom-model"))]
    {
        cell.with(operation)
    }
    #[cfg(not(all(test, feature = "loom-model")))]
    {
        operation(cell.get())
    }
}

/// Runs an exclusive mutable operation without allowing its borrow to escape the cell access.
pub(super) fn write<Payload: ?Sized, Output>(
    cell: &UnsafeCell<Payload>,
    operation: impl FnOnce(*mut Payload) -> Output,
) -> Output {
    #[cfg(all(test, feature = "loom-model"))]
    {
        cell.with_mut(operation)
    }
    #[cfg(not(all(test, feature = "loom-model")))]
    {
        operation(cell.get())
    }
}
