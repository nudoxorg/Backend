//! Java compiler authority facts transported as a validated immutable image.
//! `javac` writes fixed-width semantic planes and this crate only borrows them.
//! No JSON tree, owned DTO reconstruction, scanner fallback, or semantic string parsing occurs.
//!
//! CANONICAL AUTHORITY PATH: this module is the retained low-level authority
//! contract. Product and compiler-driver callers must reach it only through
//! the crate-level `Authority` adapter in `lib.rs`; it must never be wired in
//! as a second semantic plane.

mod bound;
pub mod central;
pub mod harness;
mod image;
pub mod jar;
pub mod purl;
pub mod repo;

pub use self::bound::{BoundHeaderError, BoundImageError, JavaAuthorityImage};
pub use self::image::{
    Atom, AtomError, AtomIter, Declaration, DeclarationExtension, DeclarationIter, DeclarationKind,
    DocFlavor, HeaderError, ImageError, ImagePlane, JavaImage, JavaRelease, Modifiers, Origin,
    Reference, ReferenceIter, SectionError, Symbol, SymbolIter, SymbolRef, TypeChildren, TypeFact,
    TypeIter, TypeKind, TypeRef,
};
