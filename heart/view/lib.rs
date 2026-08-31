//! The `heart-view` crate exists to validate and borrow canonical framed data without allocating.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![no_std]
#![deny(unsafe_code)]
//! Structural validation witnesses and allocation-free borrowed frame views.

mod validate;

pub use validate::{
    DescriptorError, Section, SectionReadError, Sections, ValidateError, ValidatedFrame,
};
