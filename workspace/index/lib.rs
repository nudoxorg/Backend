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

pub mod codec;
/// The ONE shared iroh/bao content-transfer plane (CONSOLIDATION-NOTES §8b/§8c),
/// now its OWN crate (`transport`) so `ir-vcs` can depend on it without an
/// `index ↔ ir-vcs` cycle (the cycle that would otherwise block folding the
/// server composition into `index`). Re-exported here so `index::transport::…`
/// and `crate::transport::…` paths keep resolving unchanged.
pub use ::transport as transport;
/// The per-ecosystem spec (ECOSYSTEM-PLAN): name/version/upstream/manifest/search
/// grammar per `heart::Language`. Folded in from the former standalone
/// `ecosystem` crate; used only by `index` and `driver` (which composes index),
/// so the index-free planes (`ir`, `registry`) never link it. `PackageNameExt`
/// bridges `heart::PackageName` to this grammar.
pub mod ecosystem;
pub mod engine;
pub mod entity;
pub mod enums;
pub mod ids;
pub mod ingest;
pub mod migrations;
pub mod overlays;
/// The NDPK v1 object-pack container engine (folded in from the former
/// standalone `object-pack` crate; §8: object-pack is part of the storage layer).
pub mod pack;
pub mod protocol;
pub mod resolution;
pub mod scratch;
pub mod seed_models;
pub mod store;

// ── Server-dissolve + registry-salvage (§8/§9a) ───────────────────────────────
// The data/storage/coordination layer folded out of the retired `server` crate
// and recovered from the user-deleted `registry::{blob,index,queue,runtime,…}`
// modules. Landed in dependency order; see CONSOLIDATION-NOTES §8/§9a.
/// The crate-root salvage error union (blob/store/ingest/queue/index/outbox/
/// search/resolve), each area `heart::Retryable`.
pub mod error;
/// Deterministic identity re-exports (heart-backed).
pub mod identity;
/// Package vocabulary re-exports (heart::package).
pub mod package;
/// Package metadata + facet extraction (heuristics/rich/hash).
pub mod metadata;
/// The registry↔catalog data-mapping codecs (schema::{codec,catalog_map}).
pub mod schema;
/// Read-plane runtime salvage (error surfaces; text index added later).
pub mod runtime;
/// Content-addressed package blobs: [`blob::BlobManifest`] + builder + emit.
pub mod blob;
/// The content-addressed object store (`Store<Connect-state>`) over
/// `object_store` — renamed from the deleted `registry::store` to `cas` so it
/// does not collide with this crate's catalog [`store`] module.
pub mod cas;
/// The durable scratch-backed indexing job queue (poison-pill-safe leases).
pub mod queue;
/// The object-store-backed compiled-IR lookup store (`ObjectCompiledStore`).
pub mod compiled;
/// Version resolution: name + version-request → concrete `PackageVersion`.
pub mod resolve;
/// Health/readiness probe re-exports (heart-backed).
pub mod health;
/// The shard-bakery ledger (`edgepack_artifacts` claim store); the vector-bake
/// compute is staged to `registry::vector` / the client composition (§8).
pub mod bakery;
/// `heart::sync::ContentIo` implementation for vector edge-shard artifacts:
/// [`shard_sync::ShardContentIo`] wraps the CAS store and is the seam through
/// which baked shard bytes are persisted and dep-shard installs are verified.
pub mod shard_sync;
/// Shared upstream HTTP client + catalog followers (crates/nuget pollers, §8).
pub mod upstream;
/// The global catalog store glue (`GlobalStore<Engine>` + `InstanceToken`);
/// renamed from the deleted `registry::index` to avoid the crate-name clash.
pub mod catalog;
/// Registry (package) search salvage (tantivy replica + ranking cascade).
pub mod search;
/// The transactional outbox + server-folded coordination flows.
pub mod coordination;

/// The serving composition (the dissolved `driver`/`server` crate): assembles
/// the `index` data plane + `registry` graph/vector plane into an axum server
/// (`Driver<M>`), plus config/authz/http/coordination/poll/save/bakery-worker.
/// Server-specific composition folded in per CONSOLIDATION-NOTES §7/§9c; the
/// `nudox-serve` binary is built from `server::main`. Shared client vocabulary
/// stays in `heart::client`.
///
/// Behind the `server` feature (OFF by default): keeps the default `index` build
/// a lean catalog library and keeps the `ir-vcs → zstd-seekable` symbol clash out
/// of every non-serving build. Enable with `--features server` (the `nudox-serve`
/// bin requires it).
#[cfg(feature = "server")]
pub mod server;

/// Catalog table entities (SeaORM). Alias kept so `index::tables::…` paths work.
pub use entity as tables;

/// Schema version this crate authors and reads (INDEX-PLAN ID-5).
pub const SCHEMA_VERSION: u32 = 4;

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

// ── Salvaged package vocabulary (former `registry::{Package,GlobalPackage}`) ──
// The syndication pair the queue/outbox/catalog key on. Uses `heart`'s
// deterministic `PackageId` (distinct from this crate's local `ids::PackageId`
// blob-id newtype), so the type is spelled out explicitly here.

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
