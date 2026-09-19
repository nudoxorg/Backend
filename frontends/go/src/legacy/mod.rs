//! Typed adapter for the vendored Go semantic oracle.
//! It validates borrowed binary authority images before compiler admission.
//! Legacy JSON protocol support remains isolated from the compiler boundary.
//!
//! CANONICAL AUTHORITY PATH: this `legacy` module IS the production
//! native-authority lane. The engine driver imports its symbols directly from
//! this module; there is no intervening adapter. The crate-level
//! `syntax_frontend()` constructor is the separate, documented structural
//! baseline and never substitutes for this authority.
#![allow(
    missing_docs,
    reason = "the vendored oracle protocol mirrors external JSON fields"
)]

mod image;
pub mod oracle;
pub mod staging;

pub use self::image::{
    ChanDir, ConstrainedDecl, Declaration, DeclarationKind, DocOwner, DocRow, GoImage, HeaderError,
    ImageError, MemberKind, MemberRow, MethodRow, MethodSetRow, ModuleRow, NONE, PackageRow,
    ReferenceRow, ReferenceTargetClass, ReferenceUseKind, SatisfactionRow, SignatureParameterRow,
    TypeParameterRow, TypeRow, TypeRowKind, parse_constraint_blob, split_nul,
};
pub use self::oracle::{
    ConfiguredGoOracle, GoOracle, GoOracleConfiguration, GoOracleConfigurationError,
    GoOracleExecutable, GoOracleExecutableView, OracleError, Output,
};
pub use self::staging::{StagedGoModule, StagingError, stage_module};
