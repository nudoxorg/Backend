#![deny(unsafe_code)]
#![warn(missing_docs)]
//! Filesystem adapters for durable compiler publication artifacts.

/// Canonical generation-to-manifest binding artifacts.
pub mod binding;
pub mod immutable;
/// Canonical recipe-bearing package manifest construction and validation.
pub mod manifest;
