#![feature(decl_macro)]
#![feature(macro_derive)]
#![feature(macro_metavar_expr)]
#![feature(macro_metavar_expr_concat)]

mod visitor;

pub mod builder;
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
        package::PackageInfo,
        registry::{Registry, RegistryResolver},
    };
}

pub(crate) type List<T> = Box<[T]>;
