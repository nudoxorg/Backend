#![deny(unsafe_code)]
#![warn(missing_docs)]
//! Filesystem adapters for durable compiler publication artifacts.

pub mod immutable;
/// Canonical recipe-bearing package manifest construction and validation.
pub mod manifest;
