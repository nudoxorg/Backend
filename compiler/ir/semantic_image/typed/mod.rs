//! Canonical full-image typed-pool planning.
//!
//! The façade keeps the planner internal until all full-image directories can
//! encode and validate its remapped coordinates.  Model, graph traversal, and
//! collision ordering own separate invariants so no future wire encoder has to
//! recover semantics from a monolithic helper.

use super::{canonical::CanonicalFullPlan, fault::CoreSemanticImageFault};

mod graph;
mod model;
mod order;
mod traverse;

#[cfg(test)]
mod tests;

use model::*;

pub(super) use traverse::{TypedDependencyPlan, TypedPlanError};
