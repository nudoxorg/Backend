//! Defines locality artifact behavior for `backend-store`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the locality artifact invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One-artifact sorted-row locality encoding selected by `LocalitySortedEncoding`.

mod descriptor;
mod errors;
mod header;
mod layout;
mod rank;
mod rows;
mod validate;
mod view;
mod write;

pub(in crate::root::locality) use descriptor::LocalityDescriptorWireRecord;
pub use errors::{LocalityError, LocalityWriteError};
pub use layout::{LocalityLayout, LocalityLayoutView};
pub(in crate::root::locality) use rank::member as rank_member;
pub(in crate::root::locality) use view::{
    BorrowedLanes, PlacementLanes, ProviderWire, project_descriptor,
};
pub use view::{
    LocalityValidator, ValidatedLocality, ValidatedLocalityFacts, with_validated_locality,
};
pub(crate) use write::LocalityEncoder;
pub use write::PreparedLocality;
