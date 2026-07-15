mod arena;
mod builder;
mod idx;
mod link;
mod resolver;

#[cfg(test)]
mod tests;

use crate::entry::TypedEntry;

use self::resolver::DynRegistryResolver;

// reexport at pub(crate) level to allow test_helpers to use
#[cfg(test)]
pub(crate) use self::idx::{ArenaIdx, PackageIdx};

#[cfg(not(test))]
#[expect(unused)]
use self::idx::{ArenaIdx, PackageIdx};

pub use self::{arena::EntryArena, builder::EntryBuilder, idx::{EntryIdx, RawEntryIdx}, link::EntryLink, resolver::RegistryResolver};
