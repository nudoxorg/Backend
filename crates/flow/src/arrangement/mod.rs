//! Shared arrangement facade.
//!
//! The implementation is kept behind this module boundary so callers depend
//! on one arrangement API while append/retention work can evolve independently
//! from the batch and operator kernels.

mod checkpoint;
mod core;
mod lifecycle;
mod pins;
mod read;
mod retention;
mod subscription;
mod subscriptions;

pub use checkpoint::*;
pub use core::*;
pub(crate) use pins::PinRegistry;
pub use subscription::*;
