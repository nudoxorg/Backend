//! Typed adapter for the vendored Go semantic oracle.
//! It validates borrowed binary authority images before compiler admission.
//! Legacy JSON protocol support remains isolated from the compiler boundary.
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(
    missing_docs,
    reason = "the vendored oracle protocol mirrors external JSON fields"
)]

mod image;
pub mod oracle;

pub use image::{Declaration, DeclarationIter, DeclarationKind, GoImage, HeaderError, ImageError};
pub use oracle::{GoOracle, OracleError, Output};
