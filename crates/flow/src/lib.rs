//! Incremental, typed relation flow primitives.
//!
//! The public facade is split by responsibility: the `types` module contains
//! stable identities and checked row algebra, `batch` contains immutable
//! physical batches and runs, `arrangement` owns leveled traces and
//! subscriptions, `operators` provides exact delta operators, `factor`
//! provides factorized views, and `demand` tracks exact scheduler dependencies.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod arrangement;
mod batch;
mod demand;
mod demand_lease;
mod factor;
mod materialized;
pub mod operation;
mod operators;
mod types;

pub use arrangement::*;
pub use batch::*;
pub use demand::*;
pub use demand_lease::*;
pub use factor::*;
pub use materialized::*;
pub use operators::*;
pub use types::*;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod demand_lease_tests;
