//! Java compiler authority facts transported as a validated immutable image.
//! `javac` writes fixed-width semantic planes and this crate only borrows them.
//! No JSON tree, owned DTO reconstruction, scanner fallback, or semantic string parsing occurs.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod bound;
pub mod central;
mod image;
pub mod jar;
pub mod purl;
pub mod repo;

pub use bound::{BoundHeaderError, BoundImageError, JavaAuthorityImage};
pub use image::{
    Atom, AtomError, AtomIter, Declaration, DeclarationExtension, DeclarationIter, DeclarationKind,
    DocFlavor, HeaderError, ImageError, ImagePlane, JavaImage, JavaRelease, Modifiers, Origin,
    Reference, ReferenceIter, SectionError, Symbol, SymbolIter, SymbolRef, TypeChildren, TypeFact,
    TypeIter, TypeKind, TypeRef,
};
