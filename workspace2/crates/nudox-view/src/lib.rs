#![no_std]
#![deny(unsafe_code)]
//! Structural validation witnesses and allocation-free borrowed frame views.

mod validate;

pub use validate::{
    DescriptorError, Section, SectionReadError, Sections, ValidateError, ValidatedFrame,
};
