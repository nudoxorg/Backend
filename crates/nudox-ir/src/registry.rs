mod arena;
mod builder;
mod idx;
mod resolver;

pub use self::{arena::EntryArena, builder::EntryBuilder, idx::{EntryIdx, RawEntryIdx}, resolver::RegistryResolver};
use self::{arena::EntryLink, resolver::DynRegistryResolver};
use crate::entry::TypedEntry;

#[cfg(test)]
mod tests;
