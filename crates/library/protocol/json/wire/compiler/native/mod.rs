//! Defines json wire compiler native behavior for `backend-library`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire compiler native invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Typed native compiler-work projection, split by I/O, work, worker, and terminal facts.

mod io;
mod terminal;
mod work;
mod worker;

pub(crate) use io::{NativeIoFactRef, NativeIoPhaseRef};
pub(crate) use work::NativeWorkCauseWire;
