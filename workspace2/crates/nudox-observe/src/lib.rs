#![no_std]
#![forbid(unsafe_code)]
//! Allocation-free, caller-owned observability probes.
//!
//! Event families remain in their semantic owner crates. This crate supplies only the lazy probe
//! seam and a bounded in-memory recorder suitable for deterministic tests and local diagnostics.

mod flight;
mod probe;

pub use flight::{
    DropNewest, FlightRecorder, OverwriteOldest, RecordingDisposition, RetentionPolicy,
};
pub use probe::Probe;
