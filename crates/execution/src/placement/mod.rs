//! Internal placement modules: capabilities, costs, policy, lookup, and hedge.

mod capability;
mod cost;
mod decision;
mod hedge;
mod lookup;

pub use capability::{
    LocalCapability, LocalCapabilityVerifier, LocalState, ObservationError, RemoteCapability,
    RemoteCapabilityVerifier, RemoteState, remote_state_from_capabilities,
};
pub use cost::{CompletionCost, CostObservation, CostSnapshot, CostVerifier};
pub use decision::{
    PlacementClass, PlacementDecision, PlacementPlan, PlacementPlanError, PlacementRequest,
    admit_placement_plan, choose_placement_with,
};
pub use hedge::{HedgeError, HedgeRace, HedgeSide, HedgeWinner, SharedHedgeRace};
pub use lookup::{OutputLookup, ReusableOutput, ReuseContext};
