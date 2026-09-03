//! C# oracle boundary: typed availability and source-preserving JSON replay.
//! It also validates source-bound binary Roslyn authority images.
//! Recorded fixtures prove offline terminals without semantic fallback.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod image;
mod oracle;

pub use image::{
    Atom, Attribute, CSharpImage, Declaration, DeclarationFlags, DeclarationIter, DeclarationKind,
    Doc, DocIter, GenericIter, GenericParameter, GenericSlice, HeaderError, ImageError,
    NullabilityCell, ParamIter, ParamSlice, Parameter, PartialRole, RefKind, ReferenceIter,
    ReferenceTag, ResolvedReference, Section, TypeChild, TypeChildIter, TypeNode, TypeNodeKind,
    TypeRef, VarianceTag,
};
pub use oracle::{
    CSharpOutput, DecodeError, Nullability, ToolingUnavailable, Type, decode, probe_dotnet,
    probe_dotnet_path,
};
