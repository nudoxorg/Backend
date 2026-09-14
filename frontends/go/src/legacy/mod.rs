//! Typed adapter for the vendored Go semantic oracle.
//! It validates borrowed binary authority images before compiler admission.
//! Legacy JSON protocol support remains isolated from the compiler boundary.
//!
//! CANONICAL AUTHORITY PATH: this module is the retained low-level authority
//! contract. Product and compiler-driver callers must reach it only through
//! the crate-level `Authority` adapter in `lib.rs`; it must never be wired in
//! as a second semantic plane.
#![allow(
    missing_docs,
    reason = "the vendored oracle protocol mirrors external JSON fields"
)]

mod image;
pub mod oracle;

pub use self::image::{
    ChanDir, ConstrainedDecl, Declaration, DeclarationKind, DocOwner, DocRow, GoImage, HeaderError,
    ImageError, MemberKind, MemberRow, MethodRow, MethodSetRow, ModuleRow, NONE, PackageRow,
    ReferenceRow, SatisfactionRow, SignatureParameterRow, TypeParameterRow, TypeRow, TypeRowKind,
    parse_constraint_blob, split_nul,
};
pub use self::oracle::{
    ConfiguredGoOracle, GoOracle, GoOracleConfiguration, GoOracleConfigurationError,
    GoOracleExecutable, GoOracleExecutableView, OracleError, Output,
};
