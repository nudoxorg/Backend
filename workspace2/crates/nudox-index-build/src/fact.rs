//! Compact typed facts that bridge reopened compiler entities to existing index-core rows.

mod entity;
mod key;
mod value;

pub use entity::{EntityFact, EntityFactView, EntityProjection};
pub use key::{EXACT_ENTITY_KEY_BYTES, ExactEntityKey};
pub use value::{
    ENTITY_VALUE_BYTES, ExactEntityValue, ExactEntityValueError, ExactEntityValueView,
};
