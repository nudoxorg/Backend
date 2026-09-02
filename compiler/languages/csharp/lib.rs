//! C# oracle boundary: typed availability and source-preserving JSON replay.
//! It also validates source-bound binary Roslyn authority images.
//! Recorded fixtures prove offline terminals without semantic fallback.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod image;
mod oracle;

pub use image::{
    CSharpImage, Declaration, DeclarationIter, DeclarationKind, HeaderError, ImageError,
};
pub use oracle::{
    CSharpOutput, DecodeError, Nullability, ToolingUnavailable, Type, decode, probe_dotnet,
    probe_dotnet_path,
};
