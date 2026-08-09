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
//! #     span: 0..0,
//! #     aliases: Box::new([]),
//! #     deprecation: None,
//! #     doc_links: Box::new([]),
//! #     attrs: Box::new([]),
//! #     cfg: None,
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
//! #           span: 0..0,
//! #           aliases: Box::new([]),
//! #           deprecation: None,
//! #           doc_links: Box::new([]),
//! #           attrs: Box::new([]),
//! #           cfg: None,
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
//! for child in root.children() {
//!     let child_idx = child.as_local().expect("unsealed refs are arena-local");
//!     let child = registry.resolve_entry(child_idx).await?;
//!
//!     assert_eq!(child.parent(), Some(&Ref::Local(root_idx)));
//! }
//!
//! #  Ok::<_, resolver::ResolverError>(())
//! # }).unwrap();
//! ```

#![feature(decl_macro)]
#![feature(macro_derive)]
#![feature(macro_metavar_expr)]
#![feature(macro_metavar_expr_concat)]

pub(crate) mod visitor;

pub mod apply;
pub mod body;
pub mod change;
pub mod codec;
pub mod content;
pub mod continuity;
pub mod entry;
pub mod foreign;
pub mod id;
pub mod index;
pub mod intro;
pub mod kind;
pub mod kinds;
pub mod lower;
pub mod manifest;
pub mod package;
pub mod reflect;
pub mod registry;
pub mod relation;
pub mod render;
pub mod skeleton;
pub mod view;
pub mod vocab;

pub mod prelude {
    pub use crate::{
        apply::PristineIntroTable,
        body::{
            AccessMode, BodyCall, BodyEmbed, BodyFacts, BodyImport, BodyMergeNote, ConflictPolicy,
            ControlSketch, Language, LocalBind, LocalKind, OracleAccess, OracleBody, OracleCall,
            OracleTypeMention, TreesitterBody, merge_body, overlapping_call,
        },
        change::{ContentBlake3, EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
        codec::{
            CodecError, HEADER_LEN, MAGIC_LEN, Plane, decode_body, decode_entry, encode_body,
            encode_entry, intro_id_of, ir_path, is_ir_path,
        },
        content::{ENTRY_CONTENT_DOMAIN, entry_content_hash},
        entry::{AttrTok, CfgExpr, Deprecation, DocLink, Entry, EntryInner, Node, Symbol},
        id::{PackageId, PackageIdView, UniqueId},
        index::{EntryIndex, RawRef, Ref, UntypedEntryIndex},
        kind::Kind,
        kinds::{self, *},
        lower::{Lowering, LoweringError},
        manifest::{
            BlobManifest, CasKey, ChangeId, ChangeSetRef, ChannelName, FileEntry, GenerationStamp,
            InMemoryOutbox, Outbox, OutboxEntry, OutboxError, PackageUuid, Toolchain,
            generation_stamp, manifest_stamp,
        },
        package::{IrPackage, PackageInfo},
        reflect::{ExportPolicy, exported, moniker_path, monikers},
        registry::{Registry, RegistryResolver},
        relation::{RelEnd, Relation, RelationKey, RelationSet},
        view::IrView,
        vocab::{Confidence, Occurrence, ReferenceKind, RelSpan},
    };
}

pub mod build {
    pub use crate::{
        apply::PristineIntroTable,
        body::{
            AccessMode, BodyCall, BodyEmbed, BodyFacts, BodyImport, BodyMergeNote, ConflictPolicy,
            ControlSketch, Language, LocalBind, LocalKind, OracleAccess, OracleBody, OracleCall,
            OracleTypeMention, TreesitterBody, merge_body, overlapping_call,
        },
        change::{ContentBlake3, EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
        content::{ENTRY_CONTENT_DOMAIN, entry_content_hash},
        entry::{
            AttrTok, CfgExpr, Deprecation, DocLink, Entry, EntryInner, Node, Symbol, Visibility,
        },
        id::{PackageId, UniqueId},
        index::{EntryIndex, RawRef, Ref, UntypedEntryIndex},
        intro::{Disambiguator, bootstrap_intro_id},
        kinds::{self, function::*, record::*, ty::*, *},
        lower::{Lowering, LoweringError},
        manifest::{
            BlobManifest, CasKey, ChangeId, ChangeSetRef, ChannelName, FileEntry, GenerationStamp,
            InMemoryOutbox, Outbox, OutboxEntry, OutboxError, PackageUuid, Toolchain,
            generation_stamp, manifest_stamp,
        },
        package::{EntryBuilder, IrPackage},
        reflect::{ExportPolicy, exported, moniker_path, monikers},
        relation::{RelEnd, Relation, RelationKey, RelationSet},
        skeleton::{function_signature_skeleton, trait_impl_skeleton},
        view::IrView,
        vocab::{Confidence, Occurrence, ReferenceKind, RelSpan},
    };
}

pub(crate) type List<T> = Box<[T]>;

#[cfg(test)]
mod test_helpers;
