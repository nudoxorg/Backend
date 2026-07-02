//! Registry — the interface for all registry actions and methods: storing and
//! retrieving parsed packages after computation, durable job orchestration,
//! registry (package) search, and the global index.
//!
//! ## Architecture
//! The registry is the *write plane* and durable spine of the system. It owns:
//! - the **object store** ([`store`]) — content-addressed immutable blobs;
//! - the **global index** ([`index`]) — postgres, the orchestration source of
//!   truth (package identity + [`heart::ResolutionState`]);
//! - the **job queue** ([`queue`]) — a poison-pill-safe postgres work queue;
//! - the **transactional outbox** ([`coordination`]) — fan-out intents so the
//!   derived read-plane stores (qdrant / terminus / tantivy) can poll;
//! - **ingest** ([`ingest`]) — a sanitizing extractor for *untrusted* archives;
//! - **registry search** ([`search`]) — package discovery via replica-local
//!   tantivy polled from postgres.
//!
//! ## Identity
//! All identity is minted through [`heart::package`] — deterministic UUIDv5
//! fingerprints — never hand-rolled here. This crate never re-defines
//! `PackageId`, `GlobalSymbolId`, `ContentHash`, or the lifecycle states; it
//! composes them.
#![feature(adt_const_params)]
#![feature(return_type_notation)]

pub mod blob;
pub mod catalog;
pub mod coordination;
pub mod error;
pub mod health;
pub mod identity;
pub mod index;
pub mod ingest;
pub mod metadata;
pub mod persist;
pub mod queue;
pub mod resolve;
pub mod schema;
pub mod search;
pub mod store;

use heart::{
	PackageCoordinates, Toolchain, Visibility,
	access::Tenant,
	content::Generation,
	lifecycle::ResolutionState,
	package::PackageId,
};
use serde::{Deserialize, Serialize};

pub use blob::{BlobBuilder, BlobManifest, FileEntry};
pub use catalog::{Catalog, Mutable, Plane, ReadOnly, ReadWrite};
pub use error::{BlobError, IngestError, QueueError, RegistryError, StoreError};
pub use store::Store;

/// A package as it lives in a single registry/source, before global
/// syndication.
///
/// Built on [`heart::PackageCoordinates`] rather than a bare `name` + language,
/// so "the same package" is a deterministic, offline-recomputable fact and the
/// object-store key layout is derived from a validated address. Carries the
/// build provenance ([`Toolchain`]), the access dimensions ([`Visibility`] +
/// owning [`Tenant`]) threaded through every publish/search path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
	/// The full, validated addressing tuple (origin × name × version).
	pub coordinates: PackageCoordinates,

	/// The concrete toolchain this package was analyzed against (provenance,
	/// never identity).
	pub toolchain: Toolchain,

	/// Who may see this package's records.
	pub visibility: Visibility,

	/// The tenant that owns this package record.
	pub owner: Tenant,
}

impl Package {
	/// The deterministic, system-wide identity of this package.
	///
	/// Delegates to [`PackageCoordinates::id`] — identity is never minted
	/// locally.
	pub fn id(&self) -> PackageId { self.coordinates.id() }
}

/// The final, globally-syndicated package object handed to the global index.
///
/// Carries the deterministic [`PackageId`], the per-source [`Package`] it was
/// minted from, the [`Generation`] (content hash) of the snapshot it reflects,
/// and its current lifecycle [`ResolutionState`] — the orchestration triad the
/// queue and outbox key on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalPackage {
	/// The canonical, deterministic global identity of the package.
	pub id: PackageId,

	/// The per-source record this global package was minted from.
	pub package: Package,

	/// The generation (package content hash) this record reflects.
	pub generation: Generation,

	/// Where this package currently sits in the pipeline.
	pub state: ResolutionState,
}
