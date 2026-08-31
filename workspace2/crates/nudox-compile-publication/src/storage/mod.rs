//! Closed namespace naming and single-owner immutable filesystem transitions.

mod io;
mod namespace;
mod transition;
mod types;

pub(crate) use namespace::StorageNamespace;
pub(crate) use transition::ImmutableFileStore;
pub(crate) use types::StoredFile;
pub use types::{ImmutableFileError, ImmutableIoPhase};
