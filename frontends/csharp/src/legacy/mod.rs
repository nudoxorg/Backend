//! C# oracle boundary: typed availability and source-preserving JSON replay.
//! It also validates source-bound binary Roslyn authority images.
//! Recorded fixtures prove offline terminals without semantic fallback.
//!
//! CANONICAL AUTHORITY PATH: this `legacy` module IS the production
//! native-authority lane. The engine driver imports its symbols directly from
//! this module; there is no intervening adapter. The crate-level
//! `syntax_frontend()` constructor is the separate, documented structural
//! baseline and never substitutes for this authority.

mod image;
mod oracle;
mod producer;
mod restore;
mod sdk;

pub use self::image::{
    Atom, Attribute, CSharpImage, Declaration, DeclarationFlags, DeclarationIter, DeclarationKind,
    Doc, DocIter, ForeignSourceTarget, GenericIter, GenericParameter, GenericSlice, HeaderError,
    ImageError, NullabilityCell, ParamIter, ParamSlice, Parameter, PartialRole, RefKind,
    ReferenceIter, ReferenceTag, ResolvedReference, Section, TOP_LEVEL_CONTEXT, TypeChild,
    TypeChildIter, TypeNode, TypeNodeKind, TypeRef, VarianceTag, parse_foreign_source_claim,
};
pub use self::oracle::{
    CSharpOutput, DecodeError, Nullability, ToolingUnavailable, Type, decode, probe_dotnet,
    probe_dotnet_path,
};
pub use self::producer::{
    CSHARP_CAPTURED_UNIT_POLICY_ID_V1, CSHARP_PACKAGE_CHILD_ENVIRONMENT_POLICY_ID_V1,
    CSharpAuthorityConfiguration, CSharpAuthorityControl, CSharpAuthorityError,
    CSharpAuthorityImage, CSharpAuthorityPhase, CSharpAuthorityProducer, CSharpAuthorityRequest,
    CSharpCapturedConfig, CSharpCapturedProjectGraph, CSharpCapturedProjectNode,
    CSharpCapturedSource, CSharpExecutionClosure, CSharpExecutionLease, CSharpOracle,
    CSharpSourceExecutionLease, CSharpSourceExecutionLeases, DEFAULT_IMAGE_LIMIT,
    DEFAULT_OUTPUT_LIMIT, DEFAULT_SOURCE_LIMIT,
};

pub use self::restore::{
    RestoreClosure, RestoreError, resolve_project_assets, resolve_project_assets_checked,
    resolve_project_assets_with_metadata,
};
pub use self::sdk::{
    CSharpResolvedReferenceSnapshot, CSharpResolverReceipt, CSharpSdkKind, CSharpSdkObservation,
    CSharpSdkProbe, CSharpSdkProjectProperties, CSharpTargetingReference, CSharpTargetingSnapshot,
    CSHARP_GLOBALIZATION_POLICY_ID,
    observe_csharp_sdk, observe_csharp_sdk_with, resolve_references_with_sdk,
    resolve_references_with_sdk_diagnostics,
    resolve_sdk_owned_references,
};
