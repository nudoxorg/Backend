//! Java compiler authority facts transported as a validated immutable image.
//! `javac` writes fixed-width semantic planes and this crate only borrows them.
//! No JSON tree, owned DTO reconstruction, scanner fallback, or semantic string parsing occurs.
//!
//! CANONICAL AUTHORITY PATH: this `legacy` module IS the production
//! native-authority lane. The engine driver imports its symbols directly from
//! this module; there is no intervening adapter. The crate-level
//! `syntax_frontend()` constructor is the separate, documented structural
//! baseline and never substitutes for this authority.

mod bound;
pub mod central;
pub mod harness;
mod image;
pub mod jar;
pub mod purl;
pub mod repo;
pub mod sourcepath;

pub use self::bound::{BoundHeaderError, BoundImageError, JavaAuthorityImage};
pub use self::image::{
    Atom, AtomError, AtomIter, Declaration, DeclarationExtension, DeclarationExtent,
    DeclarationIter, DeclarationKind, DocFlavor, HeaderError, ImageError, ImagePlane, JavaImage,
    JavaRelease, Modifiers, Origin, ParameterNames, Reference, ReferenceIter, ResolvedUse,
    SectionError, Symbol, SymbolIter, SymbolRef, TypeChildren, TypeFact, TypeIter, TypeKind,
    TypeRef, UseIter, UseTag,
};
