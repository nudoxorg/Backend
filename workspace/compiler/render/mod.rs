//! Item-level IR → surface-syntax rendering.
//!
//! The reverse of the language producers: where producers lower a package's
//! items *into* the shared surface IR, this module renders single IR entries
//! *back* into faithful, human-readable source syntax — one struct, one enum,
//! one function signature, one trait at a time. Whole-module and whole-package
//! emission is deliberately out of scope.
//!
//! # Architecture
//!
//! Layout is expressed with a Wadler–Lindig document algebra ([`doc`]) so that
//! *what* to print is separated from *where* to break; the printer makes every
//! line-breaking decision lazily, once it knows the column budget. On top of
//! that sits a small [`Backend`] trait plus shared layout combinators
//! ([`backend`]), and one thin `impl Backend` per target language ([`emit`]):
//! Rust, Go, Java, TypeScript, and Python. Call [`render_entry`] with a
//! [`RenderCtx`] carrying the target [`Language`].
//!
//! Item-level documentation and item names live on `ir::kind::Symbol`, not on
//! the payload structs (`Record`, `Function`, `TraitDef`, …); the entry point
//! weaves them in, and payloads carry their own inner docs (fields, variants,
//! trait methods), rendered when [`RenderOptions::show_docs`] is set.

pub mod backend;
pub mod doc;
pub mod emit;

pub use backend::{
    render_entry, render_entry_doc, Annotation, Backend, Language, RenderCtx, RenderOptions,
    Rendered,
};
