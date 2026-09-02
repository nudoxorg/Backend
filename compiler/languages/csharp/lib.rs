//! C# oracle boundary: typed availability and source-preserving JSON replay.
//! It also validates source-bound binary Roslyn authority images.
//! Recorded fixtures prove offline terminals without semantic fallback.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod image;
mod oracle;

pub use image::{
    Attribute, Atom, CSharpImage, Declaration, DeclarationFlags, DeclarationIter,
    DeclarationKind, Doc, DocIter, GenericIter, GenericParameter, GenericSlice, HeaderError,
    ImageError, NullabilityCell, ParamIter, ParamSlice, PartialRole, Parameter, ReferenceIter,
    ReferenceTag, RefKind, ResolvedReference, Section, TypeChild, TypeChildIter, TypeNode,
    TypeNodeKind, TypeRef, VarianceTag,
};
pub use oracle::{
    CSharpOutput, DecodeError, Nullability, ToolingUnavailable, Type, decode, probe_dotnet,
    probe_dotnet_path,
};
