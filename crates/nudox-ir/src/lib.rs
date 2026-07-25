//! # nudox-ir
//!
//! Core IR types, IR creation APIs, IR interaction APIs.
//!
//! ## Usage
//!
//! This crate consists of two distinct use-cases that share a set of common
//! types.
//!
//! These use-cases are _building_ up a package-level IR, and _interacting with_
//! built IRs. Note that these are treated distnictly, and there is very little
//! overlap between the APIs provided by the two; i.e., you cannot easily build
//! and then immediately use an IR.
//!
//! The common types are primarly [`Entry`](crate::entry::Entry) and it's
//! [`Kind`](crate::kind::Kind) variants---[`Record`](crate::kinds::Record),
//! [`Function`](crate::kinds::Function), etc---as well as some auxilliary
//! types, such as [`EntryIndex`](crate::index::EntryIndex).
//!
//! ### Building Package IRs
//!
//! To build an IR for a package, you use
//! [`IrPackage::build`](crate::package::IrPackage::build).
//!
//! For example:
//!
//! ```rust
//! # let sym = |n: &str| Symbol {
//! #     name: n.to_owned(),
//! #     visibility: Visibility::Public,
//! #     documentation: String::new(),
//! #     source: std::path::PathBuf::new(),
//! #     span: 0..0
//! # };
//! #
//! # let make_id = |id| id;
//! #
//! use nudox_ir::build::*;
//!
//! let pkg = IrPackage::build(PackageId::path("pkg"), sym("root"), |mut root| {
//!     // A tuple record `Point(i32, i32)` whose fields reference a shared type.
//!     root.create(make_id(1), sym("Point"), |mut rec| {
//!         let x = rec.create(make_id(2), sym("0"), |_| {
//!             Field::builder()
//!                 .key(FieldKey::Positional(0))
//!                 .ty(Type::I32)
//!                 .attributes([])
//!                 .build()
//!         });
//!
//!         let y = rec.create(make_id(3), sym("1"), |_| {
//!             Field::builder()
//!                 .key(FieldKey::Positional(1))
//!                 .ty(Type::I32)
//!                 .attributes([FieldAttribute::Mutable])
//!                 .build()
//!         });
//!
//!         Record::builder().fields([x, y]).build()
//!     });
//! });
//! ```
//!
//! ### Using IRs
//!
//! To actually _consume_ an IR, you need two things:
//!
//! 1. An implementor of
//!    [`RegistryResolver`](crate::registry::RegistryResolver).
//! 2. An actual instance of a [`Registry`](crate::registry::Registry) using
//!    said implementor.
//!
//! ```rust
//! # mod resolver {
//! #     include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/resolver.rs"));
//! # }
//! #
//! # use nudox_ir::build::*;
//! #
//! # fn sym(n: &str) -> Symbol {
//! #     Symbol {
//! #           name: n.to_owned(),
//! #           visibility: Visibility::Public,
//! #           documentation: String::new(),
//! #           source: std::path::PathBuf::new(),
//! #           span: 0..0
//! #     }
//! # };
//! #
//! # fn create_my_resolver() -> resolver::ExampleResolver {
//! #     let package = IrPackage::build(PackageId::path("pkg"), sym("root"), |mut root| {
//! #         root.create(0, sym("Record"), |_| {
//! #             Record::builder().build()
//! #         });
//! #     });
//! #     resolver::ExampleResolver::from_package(package)
//! # }
//! #
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! use nudox_ir::prelude::*;
//!
//! let resolver = create_my_resolver();
//!
//! let registry = Registry::new(resolver);
//!
//! let root_idx = registry.resolve_id_to_idx(UniqueId::root(PackageId::path("pkg")));
//!
//! let root = registry.resolve_entry(root_idx).await?;
//!
//! for &child in root.children() {
//!     let child = registry.resolve_entry(child).await?;
//!
//!     assert_eq!(child.parent(), Some(root_idx));
//! }
//!
//! #  Ok::<_, resolver::ResolverError>(())
//! # }).unwrap();
//! ```

#![feature(decl_macro)]
#![feature(macro_derive)]
#![feature(macro_metavar_expr)]
#![feature(macro_metavar_expr_concat)]

mod visitor;

pub mod entry;
pub mod id;
pub mod index;
pub mod kind;
pub mod kinds;
pub mod package;
pub mod registry;

pub mod prelude {
    pub use crate::{
        entry::{Entry, Symbol},
        id::{PackageId, PackageIdView, UniqueId},
        index::{EntryIndex, UntypedEntryIndex},
        kind::Kind,
        kinds::{self, *},
        package::{IrPackage, PackageInfo},
        registry::{Registry, RegistryResolver},
    };
}

pub mod build {
    pub use crate::{
        entry::{Entry, Symbol, Visibility},
        id::{PackageId, UniqueId},
        index::{EntryIndex, UntypedEntryIndex},
        kinds::{self, function::*, record::*, ty::*, *},
        package::{EntryBuilder, IrPackage},
    };
}

pub(crate) type List<T> = Box<[T]>;

#[cfg(test)]
mod test_helpers;
