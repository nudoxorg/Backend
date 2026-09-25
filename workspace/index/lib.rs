#![feature(return_type_notation)]
//! `index` — the versioned catalog crate (INDEX-PLAN phase IP-1).
//!
//! Two stores back the catalog:
//! - **Sovereign** `catalog.dolt` via the rusqdoltlite engine — packages,
//!   versions, generations, locations, edges, repo facts, advisories,
//!   git-monitor watermarks, outbox, overlays (schema v4, INDEX-PLAN §8, plus
//!   the three registryless tables of REGISTRYLESS-PLAN §5). All engine access
//!   funnels through the [`engine`] facade (ID-20).
//! - **Ephemeral** `scratch.sqlite` via rusqlite — jobs, wanted, sessions
//!   (writer-sticky), claims. Delete-anytime semantics; never on remotes
//!   ([`scratch`], ID-2).
//!
//! Writes flow through a single [`store::writer::CatalogWriter`] on `main`
//! (ID-1); the outbox is the only projection fan-out and is written in the same
//! transaction as the business row (ID-3); batch heartbeat commits (ID-4) are
//! explicit [`store::writer::CatalogWriter::commit_batch`] calls. Reads go
//! through the [`store::Catalog`] trait, including bitemporal
//! [`store::Catalog::at`] views resolved over the commit graph.
//!
//! Catalog tables are SeaORM entities ([`entity`]); migrations render DDL from
//! those entities (`Schema::create_table_from_entity`) behind a
//! `pre-migrate-vN` snapshot branch, with rollback = checkout ([`migrations`],
//! §13).

/// Deterministic TEXT/JSON codec for catalog enums and ids.
pub mod codec;
/// Delta-aware poll/project vocabulary (cursors, content hashes, diffs).
pub mod delta;
/// IR and vector projection that rewrites only changed content hashes.
pub mod frontier;
/// The shared iroh/bao content-transfer plane (re-exported `transport` crate).
pub use ::transport;
/// Shard-bakery ledger (`edgepack_artifacts` claim store).
pub mod bakery;
/// Content-addressed package blob manifest + builder + emit.
pub mod blob;
/// Content-addressed object store (`Store`).
pub mod cas;
/// Global catalog store glue (`GlobalStore` + `InstanceToken`).
pub mod catalog;
/// Object-store-backed compiled-IR lookup store.
pub mod compiled;
/// Transactional outbox + server-folded coordination flows.
pub mod coordination;
/// Per-ecosystem name/version/upstream/manifest grammar.
pub mod ecosystem;

pub mod edge_project;
/// The catalog engine facade (DoltLite / test-engine) behind all access.
pub mod engine;
/// SeaORM entities for the catalog tables (schema v4).
pub mod entity;
/// Codec TEXT enums plus the `TextEnum` decode trait.
pub mod enums;
/// Crate-root salvage error union (blob/store/ingest/queue/index/...).
pub mod error;
/// Health/readiness probe re-exports.
pub mod health;
/// Deterministic identity re-exports (heart-backed).
pub mod identity;
/// Catalog id newtypes (blob ids) plus heart `PackageId`.
pub mod ids;
/// Interned dependency counts and radix rank order.
pub mod lane;
/// Upstream feed and git ingestion (followers, drivers, watermarks).
pub mod ingest;
/// Package metadata and facet extraction heuristics.
pub mod metadata;
/// Schema DDL rendering plus `pre-migrate-vN` snapshot branches.
pub mod migrations;
/// Tabular overlay records and merge policies.
pub mod overlays;
/// The NDPK v1 object-pack container engine.
pub mod pack;
/// Package vocabulary re-exports (heart::package).
pub mod package;
/// The catalog op/edge protocol vocabulary.
pub mod protocol;
/// Durable scratch-backed indexing job queue.
pub mod queue;
/// Persistent identifiers: concept, version, and content stay distinct.
pub mod pid;
/// The one package-information model every ingest path emits.
pub mod record;
/// The registryless edge-resolution pass.
pub mod resolution;
/// Version resolution (name + request → `PackageVersion`).
pub mod resolve;
/// Read-plane runtime salvage (error surfaces).
pub mod runtime;
/// Registry↔catalog data-mapping codecs.
pub mod schema;
/// Ephemeral scratch.sqlite store (jobs/wanted/sessions/claims).
pub mod scratch;
/// Registry package search (tantivy replica + ranking cascade).
pub mod search;
/// Seed system-model packages and aliases.
pub mod seed_models;
/// `ContentIo` implementation for vector edge-shard artifacts.
pub mod shard_sync;
/// The `Catalog` read trait and `CatalogWriter` write facade.
pub mod store;
/// Shared upstream HTTP client + catalog followers.
pub mod upstream;

/// Serving composition (`Driver`) behind the `server` feature.
#[cfg(feature = "server")]
pub mod server;

/// SeaORM catalog entities, aliased as `tables` for `crate::tables::…` paths.
pub use entity as tables;

/// Schema version this crate authors and reads (INDEX-PLAN ID-5).
///
/// Bumped 4 -> 5 to add `idx_edges_unresolved` and the `generations`/
/// `listing_events`/`symbols_proj` version-id indexes (see
/// `migrations::ddl::schema_v4_statements`). The bump is what makes existing
/// v4 catalogs actually pick up the new indexes: [`migrations::runner`]
/// short-circuits when `current_user_version == SCHEMA_VERSION`, so without
/// this bump a catalog already at 4 would skip the migration forever and
/// never acquire the new indexes — only brand-new databases would benefit.
/// Bumping forces one more (idempotent, `IF NOT EXISTS`) DDL pass on every
/// v4 catalog, which is exactly the migration path for a set of
/// index-only changes. The runner function keeps its `migrate_to_v4` name
/// (several test files under `index/tests/` call it directly and are owned
/// by other in-flight work) but it has always migrated to `SCHEMA_VERSION`
/// dynamically, never a hardcoded 4, so the rename-free bump is safe.
pub const SCHEMA_VERSION: u32 = 5;

pub use codec::CodecError;
pub use enums::{
    AliasConfidence, CompileCacheKind, EdgeKind, EdgeSource, IrStatus, LineageEvidence,
    LineageRelation, ListingStatus, LocationStatus, OutboxOperation, ParseState, SinkKind,
    SourceKind, StoreKind, TextEnum, TextEnumError,
};
pub use ids::{
    AdvisoryId, ChannelTip, EdgepackKeyDigest, GenerationStamp, IdDecodeError, IntroIdHash,
    JobKeyHash, ObjectPackHash, PackageId, PackageStemId, StoreId,
};
pub use protocol::CatalogOp;
pub use store::{Catalog, MetaStore};

/// A package as it lives in a single registry/source, before global syndication.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Package {
    /// The full, validated addressing tuple (origin × name × version).
    pub coordinates: crate::package::Coordinates,
    /// The concrete toolchain this package was analyzed against (provenance).
    pub toolchain: heart::Toolchain,
}

impl Package {
    /// The deterministic, system-wide identity of this package.
    pub fn id(&self) -> heart::identity::PackageId {
        self.coordinates.id()
    }
}

pub use blob::{BlobBuilder, BlobManifest, FileEntry, ReferenceSet};
pub use cas::Store;
pub use error::{BlobError, IngestError, QueueError, RegistryError, StoreError};
pub use heart::content::ContentHash;

/// The final, globally-syndicated package object handed to the global catalog.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GlobalPackage {
    /// The canonical, deterministic global identity of the package.
    pub id: heart::identity::PackageId,
    /// The per-source record this global package was minted from.
    pub package: Package,
    /// Where this package currently sits in the pipeline.
    pub state: heart::ResolutionState,
    /// Derived search facets, when rich metadata has been extracted.
    #[serde(default)]
    pub facets: Option<metadata::SearchFacets>,
}
