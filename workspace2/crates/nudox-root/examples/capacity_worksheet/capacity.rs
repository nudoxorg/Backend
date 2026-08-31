//! Typed workload facts, derived deployment capacity, and machine-readable plan output.

#[path = "capacity/derive.rs"]
mod derive;
#[path = "capacity/input.rs"]
mod input;
#[path = "capacity/output.rs"]
mod output;

pub(crate) use derive::plan;
pub(crate) use input::{CapacityInput, CapacityModelError};
pub(crate) use output::CapacityPlan;
