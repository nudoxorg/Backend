//! Checked relation state and version bound transition algebra.

mod operations;
mod state;
mod transition;

pub use operations::{
    apply_delta, apply_delta_with_work, prepare_delta, prepare_delta_with_state,
    prepare_delta_with_work, prepare_internal_update_with_work,
};
pub use state::{
    CheckedStateObject, CheckedStateObjectRef, RelationEntry, RelationState, StateError,
    ValueSchema,
};
pub use transition::{
    BoundDelta, Delta, DeltaError, DeltaView, DeltaWork, MapChange, PreparedDelta,
    canonical_delta_id, checked_canonical_delta_id,
};
