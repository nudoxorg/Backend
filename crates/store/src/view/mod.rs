//! The `backend_store::view` module validates and borrows canonical framed data without allocating.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Structural validation witnesses and allocation-free borrowed frame views.

mod validate;

pub use self::validate::{
    DescriptorError, Section, SectionReadError, Sections, ValidateError, ValidatedFrame,
};
