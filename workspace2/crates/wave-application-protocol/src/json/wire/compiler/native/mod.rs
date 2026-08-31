//! Typed native compiler-work projection, split by I/O, work, worker, and terminal facts.

mod io;
mod terminal;
mod work;
mod worker;

pub(crate) use io::{NativeIoFactRef, NativeIoPhaseRef};
pub(crate) use work::NativeWorkCauseWire;
