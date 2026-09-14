//! C# oracle boundary: typed availability and source-preserving JSON replay.
//! It also validates source-bound binary Roslyn authority images.
//! Recorded fixtures prove offline terminals without semantic fallback.

mod image;
mod oracle;
mod producer;

pub use self::image::{
    Atom, Attribute, CSharpImage, Declaration, DeclarationFlags, DeclarationIter, DeclarationKind,
    Doc, DocIter, GenericIter, GenericParameter, GenericSlice, HeaderError, ImageError,
    NullabilityCell, ParamIter, ParamSlice, Parameter, PartialRole, RefKind, ReferenceIter,
    ReferenceTag, ResolvedReference, Section, TypeChild, TypeChildIter, TypeNode, TypeNodeKind,
    TypeRef, VarianceTag,
};
pub use self::oracle::{
    CSharpOutput, DecodeError, Nullability, ToolingUnavailable, Type, decode, probe_dotnet,
    probe_dotnet_path,
};
pub use self::producer::{
    CSharpAuthorityConfiguration, CSharpAuthorityControl, CSharpAuthorityError,
    CSharpAuthorityImage, CSharpAuthorityPhase, CSharpAuthorityProducer, CSharpAuthorityRequest,
    CSharpOracle, DEFAULT_IMAGE_LIMIT, DEFAULT_OUTPUT_LIMIT, DEFAULT_SOURCE_LIMIT,
};
