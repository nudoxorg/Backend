//! Defines fact behavior for `server-index-build`, whose purpose is to project reopened compiler IR into immutable exact and lexical segments.
//! This module owns the fact invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Compact typed facts that bridge reopened compiler entities to existing index-core rows.

mod entity;
mod key;
mod value;

pub use entity::{EntityFact, EntityFactView, EntityProjection};
pub use key::{EXACT_ENTITY_KEY_BYTES, ExactEntityKey};
pub(crate) use value::type_tag_code;
pub use value::{
    ENTITY_VALUE_BYTES, ExactEntityValue, ExactEntityValueError, ExactEntityValueView, IndexedType,
    LinkKinds, SemanticTypeFact,
};
