//! This module magically transforms compiler IR into JSON-LD to be dumped into a RDF triple based DB (**TerminusDB**)
//!
//! **Core goal**: *flatten nested Rust structs into RDF triple-based nodes* with stable `@id` values and minimal nesting and explicit edges (`@id` ref) instead of nested objects
//!
//! # Concepts
//!
//! ## Entry
//! An **Entry** is the symbol for the "demographics"/metadata of an API symbol
//! Think name, path, fq_name, documentation, visibility and edges to related symbols.
//!
//! Example
//! ```json
//! {
//!   "@id": "Entry/rust/jsonld/<symbol_id>",
//!   "@type": "Entry",
//!   "fq_name": "jsonld::convert::testing",
//!   "path": ["jsonld", "convert", "testing"],
//!   "visibility": "public",
//!   "kind_tag": "Function",
//!   "kind": { "@id": "Kind/rust/Function/<symbol_id>" },
//!   "members": [
//!     { "@id": "Entry/rust/jsonld/<member_symbol_id>" }
//!   ]
//! }
//! ```
//!
//! ## Kind
//! A **Kind** structurally represents a symbol. Think Function, Struct, Enum, Module, etc.
//! Kinds hold payload fields that descrie the symbol’s behavior/shape (params/return types for functions).
//!
//! There are two main categories as of now.
//! - **Hub-like kinds**: Skeleton like kind (think `Module`) that contains little to no information. The Entry would contain the significant information for a Module (members, documentation, etc.).
//! - **Authority kinds**: Lots of data within this kind (`Function`, `Struct`, `Enum`) where Kind describes the shape of this symbol
//!
//! For Shape Lookup, Kind will have some @key - hashed field, derived from contents of kind that can be used to find similar Kinds. Ignore for now, but *a clever solution could allow us to query different languages for crates that have similar solutions to a problem*.
//!
//! # Identity + URI Rules
//!
//! Requirement -> **any referencing node must be able to compute the URI of what it references** without relying on discovery order. An entry that is a `Function` that has an input parameter of Type `MyType` should be able to have the link to the Kind associated to `MyType`'s Entry.
//!
//! ## Compiler-provided symbol identity (`symbol_id`)
//! The compiler output **must provide a stable `symbol_id`** symbol. For rust, uses a hexidecimal str of i128 fingerprint-derived hash see [DefPathHash](https://doc.rust-lang.org/beta/nightly-rustc/rustc_hir/def_id/struct.DefPathHash.html)
//!
//! URI @ids:
//! - **Entry** `{"@id": "Entry/{lang}/{crate_name}/{symbol_id}}"`
//! - **Kind**  `{"@id": "Kind/{lang}/{crate_name}/{kind_tag}/{symbol_id}}"`
//!
//!
//! # Architecture
//!
//! The core idea is to store a Map of URI -> Json Values, representing the symbol/kind/etc URI and the corresponding values. Some context will be passed to contain URIs, metadata, etc. to include on various types/enum variants for Kind.
