use std::{error::Error, hash::Hash};

use crate::entry::Entry;

use crate::{
    id::{PackageId, UniqueId},
    package::PackageInfo,
};

// TODO: can we actually guarantee the [# Contract] below? If we ever want to
// support "unloading" entries, that would break this invariant

/// Provides entry data to the [`Registry`](super::Registry) on demand.
///
/// Each tool that produces or consumes IRs implements this trait to bridge
/// between the IR's abstract index space and concrete storage (files,
/// databases, network services, etc.).
///
/// # Type parameters
///
/// * `EntryId` — the tool's own identifier type for entries within a package.
///   This can be a simple `usize`, a path segment, or a domain-specific key. It
///   must implement `Eq + Hash` for use in the registry's lookup tables.
///
/// # Contract
///
/// The [`Registry`](super::Registry) guarantees that each `UniqueId` and
/// `PackageId` will be loaded at most once. After the first call, results are
/// cached. Implementors can therefore safely perform expensive work (network
/// calls, file I/O) without worrying about redundant requests.
///
/// # Concurrency
///
/// The resolver is held behind a `tokio::sync::Mutex`, so only one resolution
/// runs at a time. If your resolver is stateless or internally thread-safe,
/// consider wrapping a lock-free data structure.

pub trait RegistryResolver: Sized + 'static {
    /// The tool-specific entry identifier within a package.
    ///
    /// A type that can be used to uniquely identify an Entry within a package.
    /// Should be constructable based on information available within the IR of
    /// a package that is consuming an external package's entry as the target.
    type EntryId: Eq + Hash;

    /// The error type returned by resolution failures.
    type Error: Error;

    /// Load a single entry identified by its globally-unique id.
    ///
    /// Called when an entry's data is first requested.
    fn load_unique_id(
        &mut self,
        id: &UniqueId<Self::EntryId>,
    ) -> impl Future<Output = Result<Entry, Self::Error>>;

    // TODO: should this be documented publicly or an internal implementation
    // detail? `PackageInfo` only exposes `PackageId` publicly, the internal
    // representation of import/export tables is opaque.

    /// Load the export/import table for a package.
    ///
    /// Called once per package, the first time any entry from that package is
    /// resolved. The returned [`PackageInfo`] is used to remap serialized
    /// (export/import) indices in resolved entries to their runtime indices.
    fn load_package_info(
        &mut self,
        id: PackageId,
    ) -> impl Future<Output = Result<PackageInfo<Self::EntryId>, Self::Error>>;
}
